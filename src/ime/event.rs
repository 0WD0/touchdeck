#[derive(Debug)]
pub struct TouchDeckEvent {
    pub protocol: String,
    pub kind: String,
    pub source: String,
    pub time: u32,
    pub key: u32,
    pub state: String,
    pub modifiers: u32,
    pub translation: Option<String>,
    pub route: Option<String>,
}
