fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=assets/grabit.rc");
        println!("cargo:rerun-if-changed=assets/manifest.xml");
        println!("cargo:rerun-if-changed=assets/icons/grabit.ico");
        embed_resource::compile("assets/grabit.rc", embed_resource::NONE);
    }
}
