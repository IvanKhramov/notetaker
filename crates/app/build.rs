fn main() {
    println!("cargo:rerun-if-changed=assets/notetaker.ico");
    #[cfg(windows)]
    winres::WindowsResource::new()
        .set_icon("assets/notetaker.ico")
        .set("ProductName", "notetaker")
        .set("FileDescription", "notetaker - meeting notes")
        .compile()
        .expect("Windows icon resources");
}
