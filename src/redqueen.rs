use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct RedqueenDict {
    pub keys: Vec<Vec<u8>>,
    pub strings: Vec<Vec<u8>>,
    pub unmatched: Vec<(Vec<u8>, Vec<u8>)>,
}

const STRING: u8 = 0x00;
const ARRAY_KEY: u8 = 0x01;
const UNMATCHED: u8 = 0x02;
const PREG: u8 = 0x03;

impl RedqueenDict {
    pub fn load(path: impl AsRef<Path>) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        parse(&bytes)
    }
}

fn read_i16(bytes: &[u8], cursor: &mut usize) -> Option<i16> {
    let next = cursor.checked_add(2)?;
    if next > bytes.len() {
        return None;
    }
    let value = i16::from_le_bytes([bytes[*cursor], bytes[*cursor + 1]]);
    *cursor = next;
    Some(value)
}

fn read_bytes<'a>(bytes: &'a [u8], cursor: &mut usize, len: usize) -> Option<&'a [u8]> {
    let next = cursor.checked_add(len)?;
    if next > bytes.len() {
        return None;
    }
    let slice = &bytes[*cursor..next];
    *cursor = next;
    Some(slice)
}

const MAX_RECORDS: usize = 2048;

fn parse(bytes: &[u8]) -> RedqueenDict {
    let mut dict = RedqueenDict::default();
    let mut cursor = 0;
    while cursor < bytes.len()
        && (dict.keys.len() < MAX_RECORDS
            || dict.strings.len() < MAX_RECORDS
            || dict.unmatched.len() < MAX_RECORDS)
    {
        let kind = bytes[cursor];
        cursor += 1;
        if kind == STRING || kind == ARRAY_KEY {
            let Some(len) = read_i16(bytes, &mut cursor) else {
                break;
            };
            let len = len.max(0) as usize;
            let Some(content) = read_bytes(bytes, &mut cursor, len) else {
                break;
            };
            let owned = content.to_vec();
            dict.strings.push(owned.clone());
            if kind == ARRAY_KEY {
                dict.keys.push(owned);
            }
        } else if kind == UNMATCHED || kind == PREG {
            let Some(len1) = read_i16(bytes, &mut cursor) else {
                break;
            };
            let Some(left) = read_bytes(bytes, &mut cursor, len1.max(0) as usize) else {
                break;
            };
            let Some(len2) = read_i16(bytes, &mut cursor) else {
                break;
            };
            let Some(right) = read_bytes(bytes, &mut cursor, len2.max(0) as usize) else {
                break;
            };
            let left = left.to_vec();
            let right = right.to_vec();
            dict.strings.push(left.clone());
            dict.strings.push(right.clone());
            if kind == UNMATCHED {
                dict.unmatched.push((left, right));
            }
        } else {
            break;
        }
    }
    dict
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_and_pair() {
        let mut bytes = vec![ARRAY_KEY];
        bytes.extend(5i16.to_le_bytes());
        bytes.extend_from_slice(b"title");
        bytes.push(UNMATCHED);
        bytes.extend(1i16.to_le_bytes());
        bytes.push(b'a');
        bytes.extend(1i16.to_le_bytes());
        bytes.push(b'b');
        let dict = parse(&bytes);
        assert_eq!(dict.keys, vec![b"title".to_vec()]);
        assert_eq!(dict.unmatched, vec![(b"a".to_vec(), b"b".to_vec())]);
    }
}
