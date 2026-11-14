fn main() {
    // puts the Meld icon into meld.exe (what Explorer and the taskbar show for the file)
    #[cfg(windows)]
    {
        let _ = embed_resource::compile("meld.rc", embed_resource::NONE);
    }
}
