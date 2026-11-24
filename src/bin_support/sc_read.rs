use super::{batch_value, cli::ScRead, sc_check::read};
use bacnet_kit::sc_node::{Credentials, Node};

pub(super) fn run(args: ScRead) -> super::Result<()> {
    let files = args.files;
    let cert = read(&files.cert, "operational certificate")?;
    let key = read(&files.key, "private key")?;
    let issuers = read(&files.issuers, "issuer bundle")?;
    let crl = files
        .crl
        .as_deref()
        .map(|p| read(p, "node revocation list"))
        .transpose()?;
    let peer_crl = args
        .peer_crl
        .as_deref()
        .map(|p| read(p, "hub revocation list"))
        .transpose()?;
    let mut node = Node::new(
        args.uuid,
        Credentials {
            certificate: &cert,
            key: &key,
            issuers: &issuers,
            crl: crl.as_ref().map(|v| v.as_slice()),
            peer_crl: peer_crl.as_ref().map(|v| v.as_slice()),
        },
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let out = runtime.block_on(node.read(
        &args.primary,
        args.failover.as_deref(),
        args.target,
        &args.reference.request,
    ))?;
    if let Some(e) = out.primary_failure {
        println!("Primary attempt  {e}\nHub              failover");
    } else {
        println!("Hub              primary");
    }
    println!(
        "{}  {}",
        args.reference.label,
        batch_value(&out.reading, args.reference.request.property_identifier)
    );
    Ok(())
}
