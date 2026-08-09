#[derive(Debug)]
pub struct DecodedRender {
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
    pub width: u32,
    pub height: u32,
    pub animated: bool,
}

#[derive(Debug)]
pub struct DecodedRgba8 {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
