use anyhow::{Context, Result, bail, ensure};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use layeragent_protocol::{Evidence, now_ms};
use layeragent_sandbox::{ExecRequest, Sandbox};
use layeragent_storage::{Storage, verify_ledger};
use object::{Object, ObjectSection, ObjectSymbol};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub async fn inspect(store: &Storage, path: &Path) -> Result<Value> {
    ensure!(
        path.metadata()?.len() <= 64 * 1024 * 1024,
        "artifact exceeds the 64 MiB inspection limit"
    );
    let bytes = tokio::fs::read(path).await?;
    let sha = hex::encode(Sha256::digest(&bytes));
    let mut histogram = [0usize; 256];
    for b in &bytes {
        histogram[*b as usize] += 1;
    }
    let entropy = histogram
        .iter()
        .filter(|n| **n > 0)
        .map(|n| {
            let p = *n as f64 / bytes.len().max(1) as f64;
            -p * p.log2()
        })
        .sum::<f64>();
    let mut result =
        json!({"path":path.canonicalize()?,"sha256":sha,"bytes":bytes.len(),"entropy":entropy});
    if let Ok(file) = object::File::parse(bytes.as_slice()) {
        result["format"] = json!(format!("{:?}", file.format()));
        result["architecture"] = json!(format!("{:?}", file.architecture()));
        result["entry"] = json!(file.entry());
        result["sections"]=json!(file.sections().take(512).map(|s|json!({"name":s.name().unwrap_or("?"),"address":s.address(),"size":s.size(),"file_range":s.file_range()})).collect::<Vec<_>>());
        result["symbols"]=json!(file.symbols().take(1024).map(|s|json!({"name":s.name().unwrap_or("?"),"address":s.address(),"size":s.size()})).collect::<Vec<_>>());
    } else {
        result["format"] = json!(if bytes.starts_with(b"PK") {
            "zip/apk"
        } else if bytes.starts_with(b"dex\n") {
            "dex"
        } else if bytes.starts_with(b"\xD4\xC3\xB2\xA1") || bytes.starts_with(b"\x0A\x0D\x0D\x0A") {
            "pcap"
        } else {
            "unclassified"
        });
    }
    let record = store
        .evidence(
            "observed",
            &path.to_string_lossy(),
            &sha,
            Some(0),
            &result.to_string(),
        )
        .await?;
    result["evidence_id"] = json!(record.id);
    Ok(result)
}
#[allow(clippy::too_many_arguments)]
pub async fn bridge(
    principal: &str,
    sandbox: &dyn Sandbox,
    store: &Storage,
    workspace: &Path,
    target: &Path,
    name: &str,
    cancel: CancellationToken,
) -> Result<Value> {
    let file = target.to_string_lossy().to_string();
    let argv: Vec<String> = match name {
        "ghidra" => vec![
            "analyzeHeadless".into(),
            "/tmp/layeragent-ghidra".into(),
            "Analysis".into(),
            "-import".into(),
            file,
            "-deleteProject".into(),
        ],
        "jadx" => vec![
            "jadx".into(),
            "-d".into(),
            "/tmp/layeragent-jadx".into(),
            file,
        ],
        "ilspy" => vec!["ilspycmd".into(), file],
        "binwalk" => vec!["binwalk".into(), file],
        "unblob" => vec![
            "unblob".into(),
            "-o".into(),
            "/tmp/layeragent-unblob".into(),
            file,
        ],
        "tshark" => vec![
            "tshark".into(),
            "-r".into(),
            file,
            "-T".into(),
            "json".into(),
        ],
        "asar" => vec![
            "npx".into(),
            "--no-install".into(),
            "@electron/asar".into(),
            "list".into(),
            file,
        ],
        _ => bail!("unknown bridge; IDA/Hopper are available through configured MCP servers"),
    };
    let policy = layeragent_sandbox_policy();
    let output = sandbox
        .execute(
            ExecRequest {
                argv,
                workspace: workspace.into(),
                policy,
                stdin: None,
            },
            cancel,
            None,
        )
        .await?;
    let artifact = store
        .put_artifact(principal, serde_json::to_vec(&output)?)
        .await?;
    Ok(json!({"bridge":name,"output":output,"artifact":artifact}))
}
fn layeragent_sandbox_policy() -> layeragent_config::Policy {
    layeragent_config::Policy {
        write: false,
        timeout_secs: 300,
        ..Default::default()
    }
}
#[allow(clippy::too_many_arguments)]
pub async fn differential(
    principal: &str,
    sandbox: &dyn Sandbox,
    store: &Storage,
    workspace: &Path,
    original: Vec<String>,
    candidate: Vec<String>,
    inputs: Vec<String>,
    cancel: CancellationToken,
) -> Result<Value> {
    ensure!(
        !inputs.is_empty() && inputs.len() <= 128,
        "1..128 fixtures required"
    );
    let mut observations = vec![];
    for input in inputs {
        let mut values = vec![];
        for argv in [&original, &candidate] {
            values.push(
                sandbox
                    .execute(
                        ExecRequest {
                            argv: argv.clone(),
                            workspace: workspace.into(),
                            policy: layeragent_sandbox_policy(),
                            stdin: Some(input.clone()),
                        },
                        cancel.child_token(),
                        None,
                    )
                    .await?,
            );
        }
        let a = &values[0];
        let b = &values[1];
        let equal = !a.truncated
            && !b.truncated
            && !a.interrupted
            && !b.interrupted
            && a.code == b.code
            && a.stdout == b.stdout
            && a.stderr == b.stderr;
        observations.push(json!({"input_sha256":hex::encode(Sha256::digest(input.as_bytes())),"equal":equal,"original":a,"candidate":b}));
    }
    let matched = observations.iter().all(|v| v["equal"] == true);
    let result = json!({"verified_for_fixtures":matched,"fixtures":observations});
    let artifact = store
        .put_artifact(principal, serde_json::to_vec(&result)?)
        .await?;
    store
        .evidence(
            "observed",
            "differential-fixtures",
            artifact["artifact"].as_str().unwrap_or(""),
            None,
            &result.to_string(),
        )
        .await?;
    Ok(result)
}
pub fn seal(records: &[Evidence], private_key_hex: &str) -> Result<Value> {
    let head = verify_ledger(records)?;
    let raw = hex::decode(private_key_hex)?;
    let key = SigningKey::from_bytes(
        &raw.try_into()
            .map_err(|_| anyhow::anyhow!("signing key must contain 32 bytes"))?,
    );
    let payload = json!({"records":records.len(),"head":head,"at":now_ms()});
    let signature = key.sign(&serde_json::to_vec(&payload)?);
    Ok(
        json!({"payload":payload,"public_key":hex::encode(key.verifying_key().to_bytes()),"signature":hex::encode(signature.to_bytes())}),
    )
}
pub fn verify_seal(seal: &Value) -> Result<()> {
    let key: [u8; 32] = hex::decode(seal["public_key"].as_str().context("missing public key")?)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid key"))?;
    let sig = Signature::from_slice(&hex::decode(
        seal["signature"].as_str().context("missing signature")?,
    )?)?;
    VerifyingKey::from_bytes(&key)?.verify(&serde_json::to_vec(&seal["payload"])?, &sig)?;
    Ok(())
}
