//! Atualização: de tempos em tempos pergunta ao GitHub qual é a versão mais
//! nova e, se a pessoa clicar em Atualizar, baixa, confere e instala.
//!
//! Não instala sozinha: reiniciar no meio de um jogo apagaria o que está
//! guardado pra clipar. A consulta é só uma leitura pública do GitHub.

use crate::lang::t;
use crate::platform;
use crate::state::Shared;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const LATEST_URL: &str = "https://api.github.com/repos/facincanitech/repic/releases/latest";
/// O instalador só pode vir das versões deste repositório.
const DOWNLOAD_PREFIX: &str = "https://github.com/facincanitech/repic/releases/download/";
pub const SITE_URL: &str = "https://facincanitech.github.io/repic/";
const ASSET_NAME: &str = "RePic-Setup.exe";
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    url: String,
    sha256: String,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum UpdateState {
    #[default]
    None,
    Available(Release),
    /// Baixando ou instalando.
    Working,
    Failed,
}

/// "v0.4.10" -> (0, 4, 10). Compara como número: 0.4.10 é mais novo que 0.4.9.
fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

fn is_newer(latest: &str, current: &str) -> bool {
    matches!(
        (parse_version(latest), parse_version(current)),
        (Some(latest), Some(current)) if latest > current
    )
}

/// Lê a resposta do GitHub. Sem o instalador, o link certo e o SHA-256 oficial,
/// não há o que atualizar.
fn parse_release(json: &str) -> Option<Release> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let version = value["tag_name"].as_str()?.to_string();
    let asset = value["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some(ASSET_NAME))?;
    let url = asset["browser_download_url"].as_str()?;
    let sha256 = asset["digest"].as_str()?.strip_prefix("sha256:")?;
    let valid_hash = sha256.len() == 64 && sha256.chars().all(|c| c.is_ascii_hexdigit());
    (url.starts_with(DOWNLOAD_PREFIX) && valid_hash).then(|| Release {
        version,
        url: url.to_string(),
        sha256: sha256.to_ascii_lowercase(),
    })
}

/// O curl do próprio Windows (existe desde o Windows 10 1803).
fn curl() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("curl.exe"))
        .unwrap_or_else(|| PathBuf::from("curl.exe"))
}

fn fetch_latest() -> Result<Release, String> {
    let out = platform::hidden_command(&curl())
        .args(["-sSfL", "--max-time", "20", "-H", "User-Agent: RePic"])
        .arg(LATEST_URL)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    parse_release(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| "resposta inválida".to_string())
}

/// Procura versão nova logo depois de abrir e a cada 6 horas.
pub fn start(shared: Arc<Shared>) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(20));
        loop {
            if shared.config().check_updates {
                check(&shared);
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

fn check(shared: &Shared) {
    if *shared.update.lock().unwrap() == UpdateState::Working {
        return;
    }
    match fetch_latest() {
        Ok(release) if is_newer(&release.version, env!("CARGO_PKG_VERSION")) => {
            platform::log(&format!("atualização: {} disponível", release.version));
            *shared.update.lock().unwrap() = UpdateState::Available(release);
            shared.repaint();
        }
        Ok(_) => {}
        // Sem internet, por exemplo: tenta de novo mais tarde, sem incomodar.
        Err(e) => platform::log(&format!("atualização: não consegui consultar: {e}")),
    }
}

fn sha256_of(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn download_and_run(release: &Release) -> Result<(), String> {
    let dir = std::env::temp_dir().join("RePic").join("update");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(ASSET_NAME);
    let _ = std::fs::remove_file(&file);
    let status = platform::hidden_command(&curl())
        .args(["-sSfL", "--max-time", "900", "-H", "User-Agent: RePic", "-o"])
        .arg(&file)
        .arg(&release.url)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("download falhou ({status})"));
    }
    // O arquivo tem que ser exatamente o publicado. Antivírus também cai aqui,
    // quando apaga o arquivo logo depois de baixar.
    let hash = sha256_of(&file)?;
    if hash != release.sha256 {
        let _ = std::fs::remove_file(&file);
        return Err("o arquivo baixado não confere com o publicado".into());
    }
    // O instalador fecha o RePic (--quit) e abre a versão nova no fim.
    platform::hidden_command(&file)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// O botão Atualizar. Espera o RePic estar parado: instalar fecha o programa.
pub fn install(shared: Arc<Shared>) {
    if shared.recording.load(Ordering::SeqCst) || shared.saving.load(Ordering::SeqCst) {
        return;
    }
    let release = {
        let mut state = shared.update.lock().unwrap();
        let UpdateState::Available(release) = state.clone() else {
            return;
        };
        *state = UpdateState::Working;
        release
    };
    shared.repaint();
    std::thread::spawn(move || {
        platform::log(&format!("atualização: instalando {}", release.version));
        let result = download_and_run(&release);
        if result.is_ok() {
            // O normal é o instalador fechar o RePic antes disso.
            std::thread::sleep(Duration::from_secs(180));
        }
        let reason = result.err().unwrap_or_else(|| "o instalador não terminou".into());
        platform::log(&format!("atualização: falhou: {reason}"));
        *shared.update.lock().unwrap() = UpdateState::Failed;
        crate::recorder::notify(&shared, t().update_failed, platform::Toast::Error);
        shared.repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "00c1307c82ffbd6a0bb01d25aeb432de7ec3b132ecf0d9a8a364c03ee4e5b1b6";

    fn json(url: &str, digest: &str) -> String {
        format!(
            r#"{{"tag_name":"v0.5.0","assets":[{{"name":"RePic-Setup.exe","browser_download_url":"{url}","digest":"{digest}"}}]}}"#
        )
    }

    #[test]
    fn compares_versions_as_numbers() {
        assert!(is_newer("v0.4.10", "0.4.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("v0.4.3", "0.4.3"));
        assert!(!is_newer("v0.4.2", "0.4.3"));
        assert!(!is_newer("nightly", "0.4.3"));
    }

    #[test]
    fn reads_the_official_release() {
        let url = "https://github.com/facincanitech/repic/releases/download/v0.5.0/RePic-Setup.exe";
        let r = parse_release(&json(url, &format!("sha256:{HASH}"))).unwrap();
        assert_eq!(r.version, "v0.5.0");
        assert_eq!(r.sha256, HASH);
    }

    #[test]
    fn refuses_other_sources_and_missing_hash() {
        let good = "https://github.com/facincanitech/repic/releases/download/v0.5.0/RePic-Setup.exe";
        let evil = "https://example.com/RePic-Setup.exe";
        assert!(parse_release(&json(evil, &format!("sha256:{HASH}"))).is_none());
        assert!(parse_release(&json(good, "sha256:123")).is_none());
        assert!(parse_release(&json(good, "")).is_none());
        assert!(parse_release("não é json").is_none());
    }

    #[test]
    fn hashes_the_file() {
        let path = std::env::temp_dir().join(format!("repic-hash-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_of(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Consulta o GitHub de verdade: `cargo test --release live_update -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_update_check() {
        let release = fetch_latest().unwrap();
        println!("{} | mais nova que esta: {}", release.version, is_newer(&release.version, env!("CARGO_PKG_VERSION")));
    }
}
