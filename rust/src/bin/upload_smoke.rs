// Upload one image to a project the way the viewer does, and print the URL
// it would paste — a live check of `Client::upload_gcs` / `upload_gyazo`.
//
//   cargo run --bin upload_smoke -- <project> <image path>            # gcs
//   cargo run --bin upload_smoke -- <project> <image path> gyazo [team]
//
// Auth as the viewer: `cosense login` store, COSENSE_SID, GYAZO_*_TOKEN.
use cosense::api::{AuthStore, Client, Config};
use cosense::upload::{content_type_for, upload_gyazo, Destination};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (project, path) = match (args.first(), args.get(1)) {
        (Some(p), Some(f)) => (p.clone(), std::path::PathBuf::from(f)),
        _ => return Err("usage: upload_smoke <project> <image> [gyazo [team]]".into()),
    };
    let dest = match (args.get(2).map(String::as_str), args.get(3)) {
        (Some("gyazo"), team) => Destination::Gyazo { team: team.cloned() },
        _ => Destination::Gcs,
    };
    let sid = std::env::var("COSENSE_SID").ok().filter(|s| !s.is_empty());
    let cfg = Config { project: project.clone(), auth: AuthStore::load(sid), api_domain: "scrapbox.io".into() };
    let client = Client::new(cfg)?;
    let bytes = std::fs::read(&path)?;
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let ct = content_type_for(&path);
    println!("project settings: {:?}", client.get_project_settings(&project).ok());
    println!("→ {} ({} bytes, {ct}) to {}", name, bytes.len(), dest.label());
    let url = match &dest {
        Destination::Gcs => client.upload_gcs(&project, &bytes, &name, ct)?,
        Destination::Gyazo { team } => {
            let token = std::env::var("GYAZO_TEAMS_ACCESS_TOKEN")
                .or_else(|_| std::env::var("GYAZO_ACCESS_TOKEN"))
                .map_err(|_| "no GYAZO token")?;
            upload_gyazo(client.http(), &token, bytes, &name, ct, team.as_deref())?
        }
    };
    println!("[{url}]");
    Ok(())
}
