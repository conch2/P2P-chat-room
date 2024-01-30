use super::*;

#[derive(serde::Serialize, serde::Deserialize)]
#[derive(Debug, Default)]
pub struct Room {
    pub id: u32,
    pub name: String,
    pub passwd: String,
}

impl Room {
    pub fn new() -> Self {
        Room {
            ..Default::default()
        }
    }

    pub fn from(package: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice::<Room>(package)
    }

    pub fn to_package(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

impl ToPackage for Room {
    fn package(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

impl From<Vec<u8>> for Room {
    fn from(pkg: Vec<u8>) -> Self {
        serde_json::from_slice(pkg.as_slice()).unwrap()
    }
}
