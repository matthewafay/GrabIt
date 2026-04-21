use std::path::PathBuf;

fn main() {
    #[cfg(target_os = "windows")]
    windows_build();
}

#[cfg(target_os = "windows")]
fn windows_build() {
    println!("cargo:rerun-if-changed=assets/grabit.rc");
    println!("cargo:rerun-if-changed=assets/manifest.xml");
    println!("cargo:rerun-if-changed=assets/icons/grabit.png");
    println!("cargo:rerun-if-changed=build.rs");

    generate_ico_from_png();
    embed_resource::compile("assets/grabit.rc", embed_resource::NONE);
}

/// Decode `assets/icons/grabit.png` and write a multi-size `grabit.ico`
/// alongside it. Windows picks the closest size at display time, so we
/// supply a useful ladder: 16/24/32/48/64/128/256.
#[cfg(target_os = "windows")]
fn generate_ico_from_png() {
    use image::imageops::FilterType;

    let png_path = PathBuf::from("assets/icons/grabit.png");
    let ico_path = PathBuf::from("assets/icons/grabit.ico");

    let src = match image::open(&png_path) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            println!(
                "cargo:warning=could not read {}: {e} — shipping without an embedded icon",
                png_path.display()
            );
            return;
        }
    };

    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in sizes {
        let resized = image::imageops::resize(&src, size, size, FilterType::Lanczos3);
        let (w, h) = resized.dimensions();
        let icon_img = ico::IconImage::from_rgba_data(w, h, resized.into_raw());
        match ico::IconDirEntry::encode(&icon_img) {
            Ok(entry) => dir.add_entry(entry),
            Err(e) => println!("cargo:warning=skip ico size {size}: {e}"),
        }
    }

    let file = match std::fs::File::create(&ico_path) {
        Ok(f) => f,
        Err(e) => {
            println!("cargo:warning=cannot create {}: {e}", ico_path.display());
            return;
        }
    };
    if let Err(e) = dir.write(file) {
        println!("cargo:warning=cannot write ico: {e}");
    }
}
