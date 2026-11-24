use bacnet_kit::sc_pki::{MAX_PEM_BYTES, check};
use std::{fs::OpenOptions, io::Read, time::SystemTime};
use x509_parser::time::ASN1Time;
use zeroize::Zeroizing;

pub(super) fn read(
    path: &std::path::Path,
    role: &'static str,
) -> super::Result<Zeroizing<Vec<u8>>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .map_err(|_| format!("cannot open {role}"))?;
    let metadata = file
        .metadata()
        .map_err(|_| format!("cannot inspect {role}"))?;
    if !metadata.is_file() || metadata.len() > MAX_PEM_BYTES as u64 {
        return Err(format!("{role} must be a regular file of at most 256 KiB").into());
    }
    let mut bytes = Zeroizing::new(vec![0; metadata.len() as usize]);
    file.read_exact(&mut bytes)
        .map_err(|_| format!("cannot read {role}"))?;
    if file
        .read(&mut [0])
        .map_err(|_| format!("cannot read {role}"))?
        != 0
    {
        return Err(format!("{role} changed size during the check").into());
    }
    Ok(bytes)
}

pub(super) fn run(files: super::cli::CredentialFiles, min_days: u32) -> super::Result<()> {
    let cert = read(&files.cert, "operational certificate")?;
    let key = read(&files.key, "private key")?;
    let issuers = read(&files.issuers, "issuer bundle")?;
    let crl = files
        .crl
        .as_deref()
        .map(|p| read(p, "revocation list"))
        .transpose()?;
    let report = check(
        &cert,
        &key,
        &issuers,
        crl.as_ref().map(|v| v.as_slice()),
        SystemTime::now(),
        min_days,
    )?;
    let expiry = ASN1Time::from_timestamp(report.expires_at)?.to_rfc2822()?;
    println!(
        "Credentials ready\nLeaf SHA-256     {}\nIssuer SHA-256   {}\nValid through    {} ({} whole days)\nRevocation       {}",
        report.leaf_sha256,
        report.issuer_sha256,
        expiry,
        report.remaining_days,
        if report.crl_checked {
            "checked against supplied CRL"
        } else {
            "unknown (no CRL supplied)"
        }
    );
    Ok(())
}
