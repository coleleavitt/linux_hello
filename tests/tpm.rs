//! Seals face stores with a software TPM (swtpm) and checks that they open again only on the
//! same TPM, unmodified, and with unchanged PCRs. Needs `swtpm` on PATH. Runs without the libtest
//! harness: the TCTI is chosen through an environment variable, which must not change while
//! other threads run.

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use linux_hello::{Embedding, FaceStore};

struct Swtpm {
    child: Child,
    dir: PathBuf,
    port: u16,
}

impl Swtpm {
    fn start(name: &str) -> Result<Self> {
        let dir =
            std::env::temp_dir().join(format!("linux_hello_swtpm_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let port = free_port_pair()?;
        let child = Command::new("swtpm")
            .arg("socket")
            .arg("--tpm2")
            .arg("--tpmstate")
            .arg(format!("dir={}", dir.display()))
            .arg("--server")
            .arg(format!("type=tcp,port={port},bindaddr=127.0.0.1"))
            .arg("--ctrl")
            .arg(format!("type=tcp,port={},bindaddr=127.0.0.1", port + 1))
            .arg("--flags")
            .arg("not-need-init,startup-clear")
            .stdout(Stdio::null())
            .spawn()
            .context("starting swtpm (is it installed?)")?;
        let swtpm = Self { child, dir, port };
        let start = Instant::now();
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            ensure!(
                start.elapsed() < Duration::from_secs(5),
                "swtpm did not start listening on port {port}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(swtpm)
    }

    fn tcti(&self) -> String {
        format!("swtpm:host=127.0.0.1,port={}", self.port)
    }

    /// Make the store code talk to this TPM.
    fn select(&self) {
        std::env::set_var("TPM2TOOLS_TCTI", self.tcti());
    }

    fn extend_pcr7(&self) -> Result<()> {
        use tss_esapi::handles::PcrHandle;
        use tss_esapi::interface_types::algorithm::HashingAlgorithm;
        use tss_esapi::structures::{Digest, DigestValues};

        let mut ctx =
            tss_esapi::Context::new(tss_esapi::tcti_ldr::TctiNameConf::from_str(&self.tcti())?)?;
        let mut values = DigestValues::new();
        values.set(HashingAlgorithm::Sha256, Digest::try_from(vec![0x42; 32])?);
        ctx.execute_with_nullauth_session(|c| c.pcr_extend(PcrHandle::Pcr7, values))?;
        Ok(())
    }
}

impl Drop for Swtpm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// swtpm's control port is the server port + 1, as the swtpm TCTI expects.
fn free_port_pair() -> Result<u16> {
    for _ in 0..20 {
        let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        if port < u16::MAX && TcpListener::bind(("127.0.0.1", port + 1)).is_ok() {
            return Ok(port);
        }
    }
    bail!("no free pair of TCP ports")
}

fn template(seed: f32) -> Embedding {
    Embedding {
        features: (0..512).map(|i| (i as f32 * seed).sin()).collect(),
    }
}

fn sealed_store(path: &Path, users: &[&str], pcrs: &[u8]) -> Result<()> {
    let mut store = FaceStore::load(path)?;
    for (i, user) in users.iter().enumerate() {
        store.add(user.to_string(), template(i as f32 + 1.0));
    }
    store.seal(pcrs)?;
    store.save()
}

fn expect_error(result: Result<FaceStore>, needle: &str) -> Result<()> {
    match result {
        Ok(_) => bail!("store opened, expected an error containing {needle:?}"),
        Err(e) if format!("{e:#}").contains(needle) => Ok(()),
        Err(e) => bail!("wrong error: {e:#} (expected {needle:?})"),
    }
}

fn edit_json(path: &Path, f: impl FnOnce(&mut serde_json::Value)) -> Result<()> {
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    f(&mut value);
    std::fs::write(path, serde_json::to_vec(&value)?)?;
    Ok(())
}

/// Change one base64 character in the middle of a string field.
fn flip_base64(value: &mut serde_json::Value) {
    let s = value.as_str().unwrap().to_string();
    let mut bytes = s.into_bytes();
    let i = bytes.len() / 2;
    bytes[i] = if bytes[i] == b'A' { b'B' } else { b'A' };
    *value = serde_json::Value::String(String::from_utf8(bytes).unwrap());
}

fn round_trip(dir: &Path) -> Result<()> {
    let path = dir.join("round_trip.json");
    sealed_store(&path, &["alice", "bob"], &[])?;

    let text = std::fs::read_to_string(&path)?;
    ensure!(
        text.contains("linux_hello-tpm-v1"),
        "file is not a sealed store"
    );
    ensure!(
        !text.contains("alice") && !text.contains("features"),
        "plaintext leaked into the sealed file"
    );

    let start = Instant::now();
    let mut store = FaceStore::load(&path)?;
    println!(
        "    open (create primary + load + unseal + decrypt): {:?}",
        start.elapsed()
    );
    ensure!(store.len() == 2, "expected 2 faces, got {}", store.len());
    ensure!(
        store.get("bob").unwrap().features == template(2.0).features,
        "bob's template changed"
    );
    ensure!(
        store.sealed_pcrs() == Some(&[][..]),
        "store should be sealed without PCRs"
    );

    // Saving keeps the store sealed and under the same key.
    store.add("carol".into(), template(3.0));
    store.save()?;
    let start = Instant::now();
    store.reload()?;
    println!(
        "    reload with the key already unsealed: {:?}",
        start.elapsed()
    );
    let reopened = FaceStore::load(&path)?;
    ensure!(
        reopened.len() == 3 && reopened.get("carol").is_some(),
        "carol was not saved"
    );
    ensure!(
        !std::fs::read_to_string(&path)?.contains("carol"),
        "plaintext leaked after save"
    );
    Ok(())
}

fn unseal_to_plain(dir: &Path) -> Result<()> {
    let path = dir.join("unseal.json");
    sealed_store(&path, &["alice"], &[])?;
    let mut store = FaceStore::load(&path)?;
    store.unseal();
    store.save()?;
    let plain: std::collections::HashMap<String, Embedding> =
        serde_json::from_slice(&std::fs::read(&path)?)?;
    ensure!(
        plain["alice"].features == template(1.0).features,
        "plain store differs"
    );
    ensure!(
        FaceStore::load(&path)?.sealed_pcrs().is_none(),
        "store still sealed"
    );
    Ok(())
}

fn tampered_ciphertext(dir: &Path) -> Result<()> {
    let path = dir.join("tampered.json");
    sealed_store(&path, &["alice"], &[])?;
    edit_json(&path, |v| flip_base64(&mut v["ciphertext"]))?;
    expect_error(FaceStore::load(&path), "does not decrypt")
}

fn swapped_key(dir: &Path) -> Result<()> {
    // A valid sealed key from another store: the TPM unseals it, but it is the wrong key.
    let (a, b) = (dir.join("swap_a.json"), dir.join("swap_b.json"));
    sealed_store(&a, &["alice"], &[])?;
    sealed_store(&b, &["bob"], &[])?;
    let other_key: serde_json::Value = serde_json::from_slice(&std::fs::read(&b)?)?;
    edit_json(&a, |v| v["key"] = other_key["key"].clone())?;
    expect_error(FaceStore::load(&a), "does not decrypt")
}

fn tampered_blob(dir: &Path) -> Result<()> {
    let path = dir.join("blob.json");
    sealed_store(&path, &["alice"], &[])?;
    edit_json(&path, |v| flip_base64(&mut v["key"]["private"]))?;
    expect_error(FaceStore::load(&path), "cannot load the sealed key")
}

fn other_tpm(dir: &Path, tpm: &Swtpm) -> Result<()> {
    let path = dir.join("other_tpm.json");
    sealed_store(&path, &["alice"], &[])?;
    let other = Swtpm::start("other")?;
    other.select();
    let result = expect_error(FaceStore::load(&path), "cannot load the sealed key");
    tpm.select();
    result
}

fn pcr_policy(dir: &Path, tpm: &Swtpm) -> Result<()> {
    let path = dir.join("pcr7.json");
    sealed_store(&path, &["alice"], &[7])?;
    let store = FaceStore::load(&path)?;
    ensure!(store.sealed_pcrs() == Some(&[7][..]), "PCRs not recorded");
    tpm.extend_pcr7()?;
    expect_error(FaceStore::load(&path), "PCRs [7] changed")
}

fn tpm_unavailable(dir: &Path, tpm: &Swtpm) -> Result<()> {
    let path = dir.join("unavailable.json");
    sealed_store(&path, &["alice"], &[])?;
    let port = free_port_pair()?;
    std::env::set_var(
        "TPM2TOOLS_TCTI",
        format!("swtpm:host=127.0.0.1,port={port}"),
    );
    let result = expect_error(FaceStore::load(&path), "cannot reach the TPM");
    tpm.select();
    result
}

/// swtpm has no resource manager: leaked objects or sessions would soon fill the TPM.
fn no_leaked_handles(dir: &Path) -> Result<()> {
    let path = dir.join("round_trip.json");
    for _ in 0..20 {
        FaceStore::load(&path)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let tpm = Swtpm::start("main")?;
    tpm.select();
    let dir = std::env::temp_dir().join(format!("linux_hello_tpm_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;

    let tests: [(&str, &dyn Fn() -> Result<()>); 9] = [
        ("round_trip", &|| round_trip(&dir)),
        ("unseal_to_plain", &|| unseal_to_plain(&dir)),
        ("tampered_ciphertext", &|| tampered_ciphertext(&dir)),
        ("swapped_key", &|| swapped_key(&dir)),
        ("tampered_blob", &|| tampered_blob(&dir)),
        ("other_tpm", &|| other_tpm(&dir, &tpm)),
        ("pcr_policy", &|| pcr_policy(&dir, &tpm)),
        ("tpm_unavailable", &|| tpm_unavailable(&dir, &tpm)),
        ("no_leaked_handles", &|| no_leaked_handles(&dir)),
    ];
    let total = tests.len();
    let mut failed = 0;
    for (name, test) in tests {
        match test() {
            Ok(()) => println!("test {name} ... ok"),
            Err(e) => {
                println!("test {name} ... FAILED: {e:#}");
                failed += 1;
            }
        }
    }
    std::fs::remove_dir_all(&dir)?;
    println!("\n{} passed; {failed} failed", total - failed);
    ensure!(failed == 0, "{failed} TPM tests failed");
    Ok(())
}
