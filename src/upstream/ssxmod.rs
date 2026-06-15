//! 生成 Qwen / 阿里 Baxia 使用的 `ssxmod_itna*` 指纹 cookie。
//!
//! 这部分参考了 Go 版曾经有效的 WAF 修复：定期生成一组轻量浏览器指纹字段，
//! 再用 Baxia 的自定义 LZW/base64 字符表压缩成 cookie 值。

use rand::Rng;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CUSTOM_BASE64_CHARS: &[u8] =
    b"DGi0YA7BemWnQjCl4_bR3f8SKIF9tUz/xhr2oEOgPpac=61ZqwTudLkM5vHyNXsVJ";
const REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Default)]
struct SsxmodState {
    itna: String,
    itna2: String,
    timestamp_ms: i64,
}

#[derive(Default)]
pub struct SsxmodManager {
    state: Mutex<SsxmodState>,
}

impl SsxmodManager {
    pub fn new() -> Self {
        let manager = Self::default();
        manager.refresh();
        manager
    }

    pub fn get(&self) -> (String, String) {
        {
            let state = self.state.lock().unwrap();
            if !state.itna.is_empty()
                && !state.itna2.is_empty()
                && now_millis().saturating_sub(state.timestamp_ms)
                    < REFRESH_INTERVAL.as_millis() as i64
            {
                return (state.itna.clone(), state.itna2.clone());
            }
        }
        self.refresh();
        let state = self.state.lock().unwrap();
        (state.itna.clone(), state.itna2.clone())
    }

    fn refresh(&self) {
        let fields = process_fields(generate_fingerprint_fields());
        let itna = format!("1-{}", custom_encode(&fields.join("^")));
        let itna2_fields = [
            fields[0].clone(),
            fields[1].clone(),
            fields[23].clone(),
            "0".to_string(),
            String::new(),
            "0".to_string(),
            String::new(),
            String::new(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            fields[32].clone(),
            fields[33].clone(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
        ];
        let itna2 = format!("1-{}", custom_encode(&itna2_fields.join("^")));

        *self.state.lock().unwrap() = SsxmodState {
            itna,
            itna2,
            timestamp_ms: now_millis(),
        };
    }
}

fn generate_fingerprint_fields() -> Vec<String> {
    vec![
        random_hex(20),
        "websdk-2.3.15d".to_string(),
        "1765348410850".to_string(),
        "91".to_string(),
        "1|15".to_string(),
        "zh-CN".to_string(),
        "-480".to_string(),
        "16705151|12791".to_string(),
        "1470|956|283|797|158|0|1470|956|1470|798|0|0".to_string(),
        "5".to_string(),
        "MacIntel".to_string(),
        "10".to_string(),
        "ANGLE (Apple, ANGLE Metal Renderer: Apple M4, Unspecified Version)|Google Inc. (Apple)"
            .to_string(),
        "30|30".to_string(),
        "0".to_string(),
        "28".to_string(),
        format!("5|{}", random_hash()),
        random_hash().to_string(),
        random_hash().to_string(),
        "1".to_string(),
        "0".to_string(),
        "1".to_string(),
        "0".to_string(),
        "P".to_string(),
        "0".to_string(),
        "0".to_string(),
        "0".to_string(),
        "416".to_string(),
        "Google Inc.".to_string(),
        "8".to_string(),
        "-1|0|0|0|0".to_string(),
        random_hash().to_string(),
        "11".to_string(),
        now_millis().to_string(),
        random_hash().to_string(),
        "0".to_string(),
        rand::thread_rng().gen_range(10..=100).to_string(),
    ]
}

fn process_fields(mut fields: Vec<String>) -> Vec<String> {
    replace_split_hash(&mut fields, 16);
    replace_full_hash(&mut fields, 17);
    replace_full_hash(&mut fields, 18);
    replace_full_hash(&mut fields, 31);
    replace_full_hash(&mut fields, 34);
    fields[36] = rand::thread_rng().gen_range(10..=100).to_string();
    fields[33] = now_millis().to_string();
    fields
}

fn replace_split_hash(fields: &mut [String], idx: usize) {
    if let Some((prefix, _)) = fields[idx].split_once('|') {
        fields[idx] = format!("{}|{}", prefix, random_hash());
    }
}

fn replace_full_hash(fields: &mut [String], idx: usize) {
    fields[idx] = random_hash().to_string();
}

fn random_hash() -> u32 {
    rand::random::<u32>()
}

fn random_hex(length: usize) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| HEX[rng.gen_range(0..HEX.len())] as char)
        .collect()
}

fn custom_encode(data: &str) -> String {
    lzw_compress(data, 6, |idx| CUSTOM_BASE64_CHARS[idx] as char)
}

fn lzw_compress(data: &str, bits: usize, char_func: impl Fn(usize) -> char) -> String {
    if data.is_empty() {
        return String::new();
    }

    use std::collections::HashMap;
    let mut dict: HashMap<String, usize> = HashMap::new();
    let mut dict_to_create: HashMap<String, bool> = HashMap::new();
    let mut w = String::new();
    let mut enlarge_in = 2usize;
    let mut dict_size = 3usize;
    let mut num_bits = 2usize;
    let mut result = String::new();
    let mut value = 0usize;
    let mut position = 0usize;

    fn write_bit(
        bit: usize,
        bits: usize,
        value: &mut usize,
        position: &mut usize,
        result: &mut String,
        char_func: &impl Fn(usize) -> char,
    ) {
        *value = (*value << 1) | bit;
        if *position == bits - 1 {
            *position = 0;
            result.push(char_func(*value));
            *value = 0;
        } else {
            *position += 1;
        }
    }

    fn write_char_bits(
        mut char_code: usize,
        count: usize,
        bits: usize,
        value: &mut usize,
        position: &mut usize,
        result: &mut String,
        char_func: &impl Fn(usize) -> char,
    ) {
        for _ in 0..count {
            write_bit(char_code & 1, bits, value, position, result, char_func);
            char_code >>= 1;
        }
    }

    fn bump_width(enlarge_in: &mut usize, num_bits: &mut usize) {
        *enlarge_in -= 1;
        if *enlarge_in == 0 {
            *enlarge_in = 1usize << *num_bits;
            *num_bits += 1;
        }
    }

    for ch in data.chars() {
        let c = ch.to_string();
        if !dict.contains_key(&c) {
            dict.insert(c.clone(), dict_size);
            dict_size += 1;
            dict_to_create.insert(c.clone(), true);
        }

        let wc = format!("{}{}", w, c);
        if dict.contains_key(&wc) {
            w = wc;
            continue;
        }

        if dict_to_create.contains_key(&w) {
            let first = w.chars().next().map(|c| c as usize).unwrap_or(0);
            if first < 256 {
                for _ in 0..num_bits {
                    write_bit(0, bits, &mut value, &mut position, &mut result, &char_func);
                }
                write_char_bits(
                    first,
                    8,
                    bits,
                    &mut value,
                    &mut position,
                    &mut result,
                    &char_func,
                );
            } else {
                write_bit(1, bits, &mut value, &mut position, &mut result, &char_func);
                for _ in 1..num_bits {
                    write_bit(0, bits, &mut value, &mut position, &mut result, &char_func);
                }
                write_char_bits(
                    first,
                    16,
                    bits,
                    &mut value,
                    &mut position,
                    &mut result,
                    &char_func,
                );
            }
            bump_width(&mut enlarge_in, &mut num_bits);
            dict_to_create.remove(&w);
        } else if let Some(code) = dict.get(&w).copied() {
            write_char_bits(
                code,
                num_bits,
                bits,
                &mut value,
                &mut position,
                &mut result,
                &char_func,
            );
            bump_width(&mut enlarge_in, &mut num_bits);
        }

        dict.insert(wc, dict_size);
        dict_size += 1;
        w = c;
    }

    if !w.is_empty() {
        if dict_to_create.contains_key(&w) {
            let first = w.chars().next().map(|c| c as usize).unwrap_or(0);
            if first < 256 {
                for _ in 0..num_bits {
                    write_bit(0, bits, &mut value, &mut position, &mut result, &char_func);
                }
                write_char_bits(
                    first,
                    8,
                    bits,
                    &mut value,
                    &mut position,
                    &mut result,
                    &char_func,
                );
            } else {
                write_bit(1, bits, &mut value, &mut position, &mut result, &char_func);
                for _ in 1..num_bits {
                    write_bit(0, bits, &mut value, &mut position, &mut result, &char_func);
                }
                write_char_bits(
                    first,
                    16,
                    bits,
                    &mut value,
                    &mut position,
                    &mut result,
                    &char_func,
                );
            }
            bump_width(&mut enlarge_in, &mut num_bits);
        } else if let Some(code) = dict.get(&w).copied() {
            write_char_bits(
                code,
                num_bits,
                bits,
                &mut value,
                &mut position,
                &mut result,
                &char_func,
            );
            bump_width(&mut enlarge_in, &mut num_bits);
        }
    }

    write_char_bits(
        2,
        num_bits,
        bits,
        &mut value,
        &mut position,
        &mut result,
        &char_func,
    );
    loop {
        value <<= 1;
        if position == bits - 1 {
            result.push(char_func(value));
            break;
        }
        position += 1;
    }

    result
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::SsxmodManager;

    #[test]
    fn generated_cookies_have_expected_shape() {
        let manager = SsxmodManager::new();
        let (itna, itna2) = manager.get();

        assert!(itna.starts_with("1-"));
        assert!(itna2.starts_with("1-"));
        assert!(itna.len() > 100);
        assert!(itna2.len() > 20);
    }
}
