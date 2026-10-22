use super::*;
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Checkpoint {
    schema_version: u32,
    pub host: HostSnapshot,
    installation: Option<Vec<u8>>,
    files: Option<BTreeMap<String, Option<Vec<u8>>>>,
}
impl Checkpoint {
    pub fn has_data_snapshot(&self) -> bool {
        self.files.is_some()
    }
    pub fn new(data: &Path, host: HostSnapshot) -> Result<Self, InstallError> {
        Ok(Self {
            schema_version: 1,
            host,
            installation: read_optional(&data.join("installation.json"))?,
            files: None,
        })
    }
    pub fn capture_files(&mut self, data: &Path) -> Result<(), InstallError> {
        self.files = Some(
            SNAPSHOT_FILES
                .iter()
                .map(|name| Ok((name.to_string(), read_optional(&data.join(name))?)))
                .collect::<io::Result<_>>()?,
        );
        Ok(())
    }
    pub fn save(&self, data: &Path) -> Result<(), InstallError> {
        atomic_write(
            &data.join("installation-pending.json"),
            &serde_json::to_vec(self).map_err(|_| InstallError::InvalidState)?,
        )?;
        Ok(())
    }
    pub fn load(data: &Path) -> Result<Option<Self>, InstallError> {
        let Some(bytes) = read_optional(&data.join("installation-pending.json"))? else {
            return Ok(None);
        };
        let value: Self = serde_json::from_slice(&bytes).map_err(|_| InstallError::InvalidState)?;
        if value.schema_version != 1
            || value.files.as_ref().is_some_and(|files| {
                files.len() != SNAPSHOT_FILES.len()
                    || files
                        .keys()
                        .any(|name| !SNAPSHOT_FILES.contains(&name.as_str()))
            })
        {
            return Err(InstallError::InvalidState);
        }
        if let Some(bytes) = &value.installation {
            let old: Installation =
                serde_json::from_slice(bytes).map_err(|_| InstallError::InvalidState)?;
            if old.schema_version != 1 || !valid_version(&old.active_version) {
                return Err(InstallError::InvalidState);
            }
        }
        Ok(Some(value))
    }
    pub fn restore_files(&self, data: &Path) -> Result<(), InstallError> {
        if let Some(files) = &self.files {
            for (name, bytes) in files {
                restore_file(&data.join(name), bytes.as_deref())?;
            }
        }
        restore_file(
            &data.join("installation.json"),
            self.installation.as_deref(),
        )?;
        Ok(())
    }
}
