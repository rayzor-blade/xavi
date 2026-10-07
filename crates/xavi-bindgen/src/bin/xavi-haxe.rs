use std::path::PathBuf;

fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let usage = "usage: xavi-haxe <ash|hashlink|rayzor> <output-directory>";
    let target = args.next().and_then(|value| value.into_string().ok());
    let root = PathBuf::from(args.next().ok_or(usage)?);
    if args.next().is_some() {
        return Err(usage.into());
    }
    let runtime = match target.as_deref() {
        Some("ash" | "hashlink") => xavi_bindgen::haxe::Runtime::HashLink,
        Some("rayzor") => xavi_bindgen::haxe::Runtime::Rayzor,
        _ => return Err(usage.into()),
    };
    for file in xavi_bindgen::haxe(runtime)? {
        let path = root.join(file.path);
        std::fs::create_dir_all(path.parent().ok_or("missing output parent")?)
            .map_err(|error| error.to_string())?;
        std::fs::write(path, file.source).map_err(|error| error.to_string())?;
    }
    Ok(())
}
