use crate::compat::multipart::FileHeader;

#[derive(Debug, Clone, Default)]
pub struct OriginFile {
    pub file_info: String,
    pub user_id: String,
    pub space_id: String,
    pub topic_id: String,
    pub request_id: String,
    pub data: Option<FileHeader>,
}
