pub(crate) fn runtime_plugin_directories_are_empty() -> bool {
    // SAFETY: libheif owns the returned null-terminated pointer array. We
    // inspect only its first pointer while the allocation is valid and release
    // it exactly once with the matching libheif function.
    unsafe {
        let directories = libheif_sys::heif_get_plugin_directories();
        if directories.is_null() {
            return false;
        }
        let is_empty = (*directories).is_null();
        libheif_sys::heif_free_plugin_directories(directories);
        is_empty
    }
}
