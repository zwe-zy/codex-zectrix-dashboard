use std::path::PathBuf;
#[derive(Clone, Debug)]
pub struct CodexLocator {
    explicit: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
    search_path: Vec<PathBuf>,
}
#[derive(Clone, Debug)]
pub struct ResolvedCodex {
    pub path: PathBuf,
    pub explicit: bool,
}
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("指定的 Codex 程序不存在，请修正 CODEX_ZECTRIX_CODEX_BIN")]
    ExplicitPathInvalid,
    #[error("未找到本机 Codex，请安装并登录 Windows 版 Codex 后重试")]
    NotFound,
}
impl CodexLocator {
    pub fn new(
        explicit: Option<PathBuf>,
        local_app_data: Option<PathBuf>,
        search_path: Vec<PathBuf>,
    ) -> Self {
        Self {
            explicit,
            local_app_data,
            search_path,
        }
    }
    pub fn from_environment() -> Self {
        Self::new(
            std::env::var_os("CODEX_ZECTRIX_CODEX_BIN").map(Into::into),
            std::env::var_os("LOCALAPPDATA").map(Into::into),
            std::env::var_os("PATH")
                .map(|s| std::env::split_paths(&s).collect())
                .unwrap_or_default(),
        )
    }
    pub fn resolve(&self) -> Result<ResolvedCodex, DiscoveryError> {
        self.candidates()?
            .into_iter()
            .next()
            .ok_or(DiscoveryError::NotFound)
    }
    pub fn candidates(&self) -> Result<Vec<ResolvedCodex>, DiscoveryError> {
        if let Some(path) = &self.explicit {
            if !path.is_absolute() || !path.is_file() {
                return Err(DiscoveryError::ExplicitPathInvalid);
            }
            return Ok(vec![ResolvedCodex {
                path: path.clone(),
                explicit: true,
            }]);
        }
        let mut candidates = Vec::new();
        #[cfg(windows)]
        if let Some(root) = &self.local_app_data {
            candidates.extend(crate::app_server::find_installed_codex_candidates(root));
        }
        #[cfg(not(windows))]
        let _ = &self.local_app_data;
        let name = if cfg!(windows) { "codex.exe" } else { "codex" };
        for path in self
            .search_path
            .iter()
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(name))
            .filter(|path| path.is_file())
        {
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
        if candidates.is_empty() {
            return Err(DiscoveryError::NotFound);
        }
        Ok(candidates
            .into_iter()
            .map(|path| ResolvedCodex {
                path,
                explicit: false,
            })
            .collect())
    }
}
