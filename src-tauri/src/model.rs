#[derive(Debug)]
pub(crate) struct DecodedRender {
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
    pub width: u32,
    pub height: u32,
    pub animated: bool,
}
