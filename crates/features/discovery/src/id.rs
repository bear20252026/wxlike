use rand::RngCore;

const ID_BYTES: usize = 16;

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", random_hex(ID_BYTES))
}

pub fn random_hex(byte_len: usize) -> String {
    let mut bytes = vec![0u8; byte_len];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
