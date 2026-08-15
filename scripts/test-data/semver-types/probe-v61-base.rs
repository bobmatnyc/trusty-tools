pub struct S;
impl S {
    pub fn method_ret(&self) -> u64 {
        0
    }
    pub fn method_param(&self, x: u64) -> u64 {
        x
    }
    pub async fn async_ret(&self) -> Vec<u64> {
        Vec::new()
    }
}
pub fn free_ret() -> u64 {
    0
}
pub fn free_param(x: u64) -> u64 {
    x
}
pub enum E {
    A,
}
pub struct F {
    pub f: u64,
}
pub const C: u64 = 1;
pub fn removed_fn() {}
pub trait T {
    fn tm(&self) -> u64;
}
