fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    xavi_backend::install(&out).unwrap();
    let model = xavi_bindgen::generate(xavi_bindgen::Runtime::Rayzor).unwrap();
    std::fs::write(out.join("media.rs"), model).unwrap();
}
