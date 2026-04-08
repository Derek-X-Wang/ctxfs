use super::DetectedDep;
use anyhow::Result;
use std::path::Path;

#[allow(clippy::unnecessary_wraps)] // stub; real impl will propagate parse errors
pub fn parse_requirements_txt(_path: &Path) -> Result<Vec<DetectedDep>> {
    Ok(Vec::new())
}

#[allow(clippy::unnecessary_wraps)] // stub; real impl will propagate parse errors
pub fn parse_pyproject_toml(_path: &Path) -> Result<Vec<DetectedDep>> {
    Ok(Vec::new())
}
