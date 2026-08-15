pub struct S;
impl S {
    pub fn method_ret(&self) -> Result<u64, String> {
        Ok(0)
    }
    pub fn method_param(&self, x: String) -> u64 {
        x.len() as u64
    }
}
pub fn free_ret() -> Result<u64, String> {
    Ok(0)
}
pub fn free_param(x: String) -> u64 {
    x.len() as u64
}
pub enum E {
    A,
    B,
}
pub struct F {
    pub f: i64,
}
pub const C: i64 = 1;
pub trait T {
    fn tm(&self) -> Result<u64, String>;
}
