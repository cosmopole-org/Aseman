/// The security driver interface — key management, encryption, auth.
pub trait ISecurity: Send + Sync {
    fn load_keys(&self);
    fn generate_secure_key_pair(&self, tag: &str);
    fn fetch_key_pair(&self, tag: &str) -> Vec<Vec<u8>>;
    fn encrypt(&self, tag: &str, plain_text: &str) -> String;
    fn decrypt(&self, tag: &str, cipher_text: &str) -> String;
    /// Returns `(authenticated, resolvedUserId, isGod)`.
    fn auth_with_signature(
        &self,
        user_id: &str,
        packet: &[u8],
        signature_base64: &str,
    ) -> (bool, String, bool);
    fn has_access_to_store(&self, user_id: &str, store_id: &str) -> bool;
}
