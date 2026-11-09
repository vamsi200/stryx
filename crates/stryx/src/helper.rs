use xorf::{BinaryFuse8, BinaryFuse16, BinaryFuse32, Descriptor};

use crate::{
    builder::{FilterSection, FilterTypeRepr, XorFilter},
    decoder::Decoder,
    error::{DecodeError, StryxError},
};

fn encode_fuse_data<T, F>(
    seed: u64,
    segment_length: u32,
    segment_length_mask: u32,
    segment_count_length: u32,
    fingerprints: &[T],
    item_size: usize,
    write_fingerprints: F,
) -> Result<Option<FilterSection>, StryxError>
where
    F: FnOnce(&mut Vec<u8>),
{
    let fp_len = u32::try_from(fingerprints.len())?;
    let header_size = 8 + 4 + 4 + 4 + 4;
    let mut encoded = Vec::with_capacity(header_size + fingerprints.len() * item_size);

    encoded.extend_from_slice(&seed.to_le_bytes());
    encoded.extend_from_slice(&segment_length.to_le_bytes());
    encoded.extend_from_slice(&segment_length_mask.to_le_bytes());
    encoded.extend_from_slice(&segment_count_length.to_le_bytes());
    encoded.extend_from_slice(&fp_len.to_le_bytes());

    write_fingerprints(&mut encoded);

    let checksum = crc32c::crc32c(&encoded);

    Ok(Some(FilterSection {
        data: encoded,
        checksum,
    }))
}

pub(crate) fn filter_encode(filter: XorFilter) -> Result<Option<FilterSection>, StryxError> {
    match filter {
        XorFilter::None => Ok(None),
        XorFilter::BinaryFuse8(bf) => encode_fuse_data(
            bf.descriptor.seed,
            bf.descriptor.segment_length,
            bf.descriptor.segment_length_mask,
            bf.descriptor.segment_count_length,
            &bf.fingerprints,
            1,
            |encoded| encoded.extend_from_slice(&bf.fingerprints),
        ),
        XorFilter::BinaryFuse16(bf) => encode_fuse_data(
            bf.descriptor.seed,
            bf.descriptor.segment_length,
            bf.descriptor.segment_length_mask,
            bf.descriptor.segment_count_length,
            &bf.fingerprints,
            2,
            |encoded| {
                for fingerprint in bf.fingerprints.iter() {
                    encoded.extend_from_slice(&fingerprint.to_le_bytes());
                }
            },
        ),
        XorFilter::BinaryFuse32(bf) => encode_fuse_data(
            bf.descriptor.seed,
            bf.descriptor.segment_length,
            bf.descriptor.segment_length_mask,
            bf.descriptor.segment_count_length,
            &bf.fingerprints,
            4,
            |encoded| {
                for fingerprint in bf.fingerprints.iter() {
                    encoded.extend_from_slice(&fingerprint.to_le_bytes());
                }
            },
        ),
    }
}

trait FuseFingerprint: Sized {
    const SIZE: usize;
    fn from_le_slice(slice: &[u8]) -> Self;
}

impl FuseFingerprint for u8 {
    const SIZE: usize = 1;
    fn from_le_slice(slice: &[u8]) -> Self {
        slice[0]
    }
}

impl FuseFingerprint for u16 {
    const SIZE: usize = 2;
    fn from_le_slice(slice: &[u8]) -> Self {
        u16::from_le_bytes([slice[0], slice[1]])
    }
}

impl FuseFingerprint for u32 {
    const SIZE: usize = 4;
    fn from_le_slice(slice: &[u8]) -> Self {
        u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])
    }
}

fn decode_fuse_common<T: FuseFingerprint>(
    data: &[u8],
) -> Result<(Descriptor, Box<[T]>), StryxError> {
    let mut d = Decoder::new(data);
    let seed = d.read_u64()?;
    let segment_length = d.read_u32()?;
    let segment_length_mask = d.read_u32()?;
    let segment_count_length = d.read_u32()?;
    let fp_len = d.read_u32()? as usize;

    if !segment_length.is_power_of_two() || segment_length_mask != segment_length - 1 {
        return Err(DecodeError::InvalidLength.into());
    }

    // fingerprints must cover (segment_count + 2) segments; segment_count_length = segment_count * segment_length
    let min_len = (segment_count_length as usize)
        .checked_add(2 * segment_length as usize)
        .ok_or(DecodeError::InvalidLength)?;

    if fp_len < min_len {
        return Err(DecodeError::InvalidLength.into());
    }

    let byte_len = fp_len
        .checked_mul(T::SIZE)
        .ok_or(DecodeError::InvalidLength)?;
    if d.remaining() != byte_len {
        return Err(DecodeError::InvalidLength.into());
    }

    let fingerprints: Box<[T]> = data[data.len() - byte_len..]
        .chunks_exact(T::SIZE)
        .map(T::from_le_slice)
        .collect();

    Ok((
        Descriptor {
            seed,
            segment_length_mask,
            segment_length,
            segment_count_length,
        },
        fingerprints,
    ))
}

fn decode_fuse8(data: &[u8]) -> Result<BinaryFuse8, StryxError> {
    let (descriptor, fingerprints) = decode_fuse_common::<u8>(data)?;
    Ok(BinaryFuse8 {
        fingerprints,
        descriptor,
    })
}

fn decode_fuse16(data: &[u8]) -> Result<BinaryFuse16, StryxError> {
    let (descriptor, fingerprints) = decode_fuse_common::<u16>(data)?;
    Ok(BinaryFuse16 {
        fingerprints,
        descriptor,
    })
}

fn decode_fuse32(data: &[u8]) -> Result<BinaryFuse32, StryxError> {
    let (descriptor, fingerprints) = decode_fuse_common::<u32>(data)?;
    Ok(BinaryFuse32 {
        fingerprints,
        descriptor,
    })
}

pub(crate) fn filter_decode(
    data: &[u8],
    selected_type: FilterTypeRepr,
) -> Result<XorFilter, StryxError> {
    match selected_type {
        FilterTypeRepr::None => Ok(XorFilter::None),
        FilterTypeRepr::BinaryFuse8 => Ok(XorFilter::BinaryFuse8(decode_fuse8(data)?)),
        FilterTypeRepr::BinaryFuse16 => Ok(XorFilter::BinaryFuse16(decode_fuse16(data)?)),
        FilterTypeRepr::BinaryFuse32 => Ok(XorFilter::BinaryFuse32(decode_fuse32(data)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xorf::Filter as XorfFilter;

    // Header layout: seed(8) | seg_len(4) | mask(4) | seg_count_len(4) | fp_len(4) | payload
    const OFF_SEG_LEN: usize = 8;
    const OFF_MASK: usize = 12;
    const OFF_FP_LEN: usize = 20;
    const HEADER: usize = 24;

    fn hashes(n: u64) -> Vec<u64> {
        (0..n)
            .map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15))
            .collect()
    }

    fn absent_keys() -> Vec<u64> {
        (0..1000u64)
            .map(|i| (i + 1_000_000).wrapping_mul(0xD6E8_FEB8_6659_FD93))
            .collect()
    }

    fn encode8(keys: &[u64]) -> (BinaryFuse8, Vec<u8>) {
        let bf = BinaryFuse8::try_from(&keys.to_vec()).unwrap();
        let section = filter_encode(XorFilter::BinaryFuse8(bf.clone()))
            .unwrap()
            .unwrap();
        (bf, section.data)
    }

    fn encode16(keys: &[u64]) -> (BinaryFuse16, Vec<u8>) {
        let bf = BinaryFuse16::try_from(&keys.to_vec()).unwrap();
        let section = filter_encode(XorFilter::BinaryFuse16(bf.clone()))
            .unwrap()
            .unwrap();
        (bf, section.data)
    }

    fn put_u32(buf: &mut [u8], off: usize, v: u32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn fuse8_round_trip() {
        let keys = hashes(10_000);
        let (orig, bytes) = encode8(&keys);
        let decoded = decode_fuse8(&bytes).unwrap();

        assert_eq!(decoded.fingerprints, orig.fingerprints);
        for k in &keys {
            assert!(decoded.contains(k), "false negative for {k}");
        }

        // false positives are expected
        for k in absent_keys() {
            assert_eq!(decoded.contains(&k), orig.contains(&k));
        }
    }

    #[test]
    fn fuse16_round_trip() {
        let keys = hashes(10_000);
        let (orig, bytes) = encode16(&keys);
        let decoded = decode_fuse16(&bytes).unwrap();

        assert_eq!(decoded.fingerprints, orig.fingerprints);
        for k in &keys {
            assert!(decoded.contains(k), "false negative for {k}");
        }
        for k in absent_keys() {
            assert_eq!(decoded.contains(&k), orig.contains(&k));
        }
    }

    #[test]
    fn round_trip_small_sets() {
        for n in [1u64, 2, 3, 10, 100] {
            let keys = hashes(n);

            let (_, b8) = encode8(&keys);
            let d8 = decode_fuse8(&b8).unwrap();
            assert!(keys.iter().all(|k| d8.contains(k)), "fuse8 n={n}");

            let (_, b16) = encode16(&keys);
            let d16 = decode_fuse16(&b16).unwrap();
            assert!(keys.iter().all(|k| d16.contains(k)), "fuse16 n={n}");
        }
    }

    #[test]
    fn round_trip_many_sizes() {
        for n in [1usize, 2, 5, 50, 500, 5_000, 50_000, 500_000] {
            let keys = hashes(n as u64);

            let (_, b8) = encode8(&keys);
            let d8 =
                decode_fuse8(&b8).unwrap_or_else(|e| panic!("fuse8 decode failed at n={n}: {e:?}"));
            assert!(
                keys.iter().all(|k| d8.contains(k)),
                "fuse8 false negative at n={n}"
            );

            let (_, b16) = encode16(&keys);
            let d16 = decode_fuse16(&b16)
                .unwrap_or_else(|e| panic!("fuse16 decode failed at n={n}: {e:?}"));
            assert!(
                keys.iter().all(|k| d16.contains(k)),
                "fuse16 false negative at n={n}"
            );
        }
    }

    #[test]
    fn fuse8_rejects_truncated_payload() {
        let (_, mut bytes) = encode8(&hashes(1000));
        bytes.pop();
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse8_rejects_trailing_bytes() {
        let (_, mut bytes) = encode8(&hashes(1000));
        bytes.push(0);
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse8_rejects_truncated_header() {
        let (_, bytes) = encode8(&hashes(1000));
        assert!(decode_fuse8(&bytes[..HEADER - 1]).is_err());
        assert!(decode_fuse8(&[]).is_err());
    }

    #[test]
    fn fuse8_rejects_oversized_fp_len() {
        let (_, mut bytes) = encode8(&hashes(1000));
        put_u32(&mut bytes, OFF_FP_LEN, u32::MAX);
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse8_rejects_non_power_of_two_segment_length() {
        let (_, mut bytes) = encode8(&hashes(1000));
        put_u32(&mut bytes, OFF_SEG_LEN, 3);
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse8_rejects_mask_mismatch() {
        let (_, mut bytes) = encode8(&hashes(1000));
        put_u32(&mut bytes, OFF_MASK, 0);
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse8_rejects_too_few_fingerprints() {
        let (_, mut bytes) = encode8(&hashes(1000));
        put_u32(&mut bytes, OFF_FP_LEN, 0);
        bytes.truncate(HEADER);
        assert!(decode_fuse8(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_truncated_payload() {
        let (_, mut bytes) = encode16(&hashes(1000));
        bytes.pop();
        assert!(decode_fuse16(&bytes).is_err());
        bytes.pop();
        assert!(decode_fuse16(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_trailing_bytes() {
        let (_, mut bytes) = encode16(&hashes(1000));
        bytes.extend_from_slice(&[0, 0]);
        assert!(decode_fuse16(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_element_count_used_as_byte_count() {
        let (_, mut bytes) = encode16(&hashes(1000));
        let fp_len = u32::from_le_bytes(bytes[OFF_FP_LEN..OFF_FP_LEN + 4].try_into().unwrap());
        bytes.truncate(HEADER + fp_len as usize);
        assert!(decode_fuse16(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_oversized_fp_len() {
        let (_, mut bytes) = encode16(&hashes(1000));
        put_u32(&mut bytes, OFF_FP_LEN, u32::MAX);
        assert!(decode_fuse16(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_non_power_of_two_segment_length() {
        let (_, mut bytes) = encode16(&hashes(1000));
        put_u32(&mut bytes, OFF_SEG_LEN, 3);
        assert!(decode_fuse16(&bytes).is_err());
    }

    #[test]
    fn fuse16_rejects_too_few_fingerprints() {
        let (_, mut bytes) = encode16(&hashes(1000));
        put_u32(&mut bytes, OFF_FP_LEN, 0);
        bytes.truncate(HEADER);
        assert!(decode_fuse16(&bytes).is_err());
    }

    #[test]
    fn test_binary_fuse32_encode_decode_roundtrip() {
        let descriptor = Descriptor {
            seed: 0xDEADBEEFCAFE,
            segment_length: 4,
            segment_length_mask: 3,
            segment_count_length: 4,
        };

        let fingerprints: Box<[u32]> = vec![
            0x11223344, 0x55667788, 0x99AABBCC, 0xDDEEFF00, 0x01020304, 0x05060708, 0x090A0B0C,
            0x0D0E0F10, 0x12345678, 0x87654321, 0xA1B2C3D4, 0xD4C3B2A1,
        ]
        .into_boxed_slice();

        let fuse32 = BinaryFuse32 {
            descriptor: descriptor.clone(),
            fingerprints: fingerprints.clone(),
        };

        let encoded = filter_encode(XorFilter::BinaryFuse32(fuse32))
            .expect("encoding should succeed")
            .expect("section should be present");

        assert_eq!(crc32c::crc32c(&encoded.data), encoded.checksum);

        let decoded = filter_decode(&encoded.data, FilterTypeRepr::BinaryFuse32)
            .expect("decoding should succeed");

        match decoded {
            XorFilter::BinaryFuse32(decoded_fuse32) => {
                assert_eq!(decoded_fuse32.descriptor.seed, descriptor.seed);
                assert_eq!(
                    decoded_fuse32.descriptor.segment_length,
                    descriptor.segment_length
                );
                assert_eq!(
                    decoded_fuse32.descriptor.segment_length_mask,
                    descriptor.segment_length_mask
                );
                assert_eq!(
                    decoded_fuse32.descriptor.segment_count_length,
                    descriptor.segment_count_length
                );
                assert_eq!(decoded_fuse32.fingerprints, fingerprints);
            }
            _ => panic!("decoded filter was not BinaryFuse32"),
        }
    }

    #[test]
    fn test_binary_fuse32_decode_invalid_length() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u64.to_le_bytes());
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(&12u32.to_le_bytes());

        data.extend_from_slice(&0x11223344u32.to_le_bytes());

        let result = decode_fuse32(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_binary_fuse32_decode_invalid_segment_mask() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u64.to_le_bytes());
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&4u32.to_le_bytes());
        data.extend_from_slice(&12u32.to_le_bytes());

        for _ in 0..12 {
            data.extend_from_slice(&0u32.to_le_bytes());
        }

        let result = decode_fuse32(&data);
        assert!(result.is_err());
    }
}
