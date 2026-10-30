//! Who is behind an IP address: the network (ASN, owner, country) from an offline copy of the
//! public-domain iptoasn.com database, and whether the site sits behind a CDN / reverse proxy.
//! Lookups never leave the machine — asking an online service would itself leak every site
//! you visit.

mod classify;

pub use classify::{Edge, classify};

use std::{
    collections::HashMap,
    fs,
    io::Read,
    net::IpAddr,
    path::PathBuf,
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};

const URL: &str = "https://iptoasn.com/data/ip2asn-combined.tsv.gz";
const FILE: &str = "ip2asn-combined.tsv.gz";
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asn {
    pub number: u32,
    /// Registry name, e.g. `CLOUDFLARENET`.
    pub name: String,
    pub country: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Loading,
    Ready,
    Unavailable,
}

#[derive(Default)]
struct Table {
    v4: Vec<(u32, u32, u32)>,
    v6: Vec<(u128, u128, u32)>,
    /// ASN → (name, country).
    owners: HashMap<u32, (String, String)>,
}

pub struct NetInfo {
    table: RwLock<Option<Table>>,
    status: RwLock<Status>,
}

impl NetInfo {
    /// Loads the cached database from `dir` (downloading it when missing or a week old) on a
    /// background thread; `on_ready` fires when lookups start working.
    pub fn new(dir: PathBuf, on_ready: impl Fn() + Send + 'static) -> Arc<Self> {
        let info = Arc::new(Self { table: RwLock::new(None), status: RwLock::new(Status::Loading) });
        let worker = info.clone();
        thread::Builder::new()
            .name("netinfo".into())
            .spawn(move || {
                let status = match load(&dir) {
                    Ok(table) => {
                        log::info!("netinfo: {} IPv4 + {} IPv6 ranges", table.v4.len(), table.v6.len());
                        *worker.table.write().unwrap() = Some(table);
                        Status::Ready
                    }
                    Err(e) => {
                        log::warn!("netinfo: {e}");
                        Status::Unavailable
                    }
                };
                *worker.status.write().unwrap() = status;
                on_ready();
            })
            .expect("spawn netinfo thread");
        info
    }

    pub fn status(&self) -> Status {
        *self.status.read().unwrap()
    }

    pub fn lookup(&self, ip: IpAddr) -> Option<Asn> {
        let table = self.table.read().unwrap();
        table.as_ref()?.lookup(ip)
    }
}

impl Table {
    fn lookup(&self, ip: IpAddr) -> Option<Asn> {
        let number = match ip {
            IpAddr::V4(v4) => find(&self.v4, u32::from(v4)),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => find(&self.v4, u32::from(v4)),
                None => find(&self.v6, u128::from(v6)),
            },
        }?;
        let (name, country) = self.owners.get(&number)?;
        Some(Asn { number, name: name.clone(), country: country.clone() })
    }
}

/// Binary search over sorted, non-overlapping `(start, end, asn)` ranges.
fn find<T: Ord + Copy>(ranges: &[(T, T, u32)], ip: T) -> Option<u32> {
    let i = ranges.partition_point(|&(start, _, _)| start <= ip).checked_sub(1)?;
    let (_, end, asn) = ranges[i];
    (ip <= end && asn != 0).then_some(asn)
}

fn load(dir: &std::path::Path) -> Result<Table, String> {
    let path = dir.join(FILE);
    let fresh = fs::metadata(&path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or_default() < MAX_AGE);
    if !fresh {
        match download() {
            Ok(bytes) => {
                let _ = fs::create_dir_all(dir);
                fs::write(&path, &bytes).map_err(|e| e.to_string())?;
            }
            Err(e) if path.exists() => log::warn!("netinfo: refresh failed, using the old copy: {e}"),
            Err(e) => return Err(e),
        }
    }
    let gz = fs::read(&path).map_err(|e| e.to_string())?;
    let mut text = String::new();
    flate2::read::GzDecoder::new(&gz[..]).read_to_string(&mut text).map_err(|e| e.to_string())?;
    Ok(parse(&text))
}

fn download() -> Result<Vec<u8>, String> {
    let response = ureq::get(URL).call().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    response.into_body().into_reader().take(64 << 20).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// `range_start \t range_end \t asn \t country \t description` per line.
fn parse(text: &str) -> Table {
    let mut table = Table::default();
    for line in text.lines() {
        let mut cols = line.split('\t');
        let (Some(start), Some(end), Some(asn), Some(country), Some(name)) =
            (cols.next(), cols.next(), cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        let Ok(asn) = asn.parse::<u32>() else { continue };
        match (start.parse::<IpAddr>(), end.parse::<IpAddr>()) {
            (Ok(IpAddr::V4(s)), Ok(IpAddr::V4(e))) => table.v4.push((s.into(), e.into(), asn)),
            (Ok(IpAddr::V6(s)), Ok(IpAddr::V6(e))) => table.v6.push((s.into(), e.into(), asn)),
            _ => continue,
        }
        if asn != 0 {
            table.owners.entry(asn).or_insert_with(|| (name.to_owned(), country.to_owned()));
        }
    }
    table.v4.sort_unstable();
    table.v6.sort_unstable();
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "1.0.0.0\t1.0.0.255\t13335\tUS\tCLOUDFLARENET\n\
                          1.0.1.0\t1.0.3.255\t0\tNone\tNot routed\n\
                          8.8.8.0\t8.8.8.255\t15169\tUS\tGOOGLE\n\
                          2606:4700::\t2606:4700:ffff:ffff:ffff:ffff:ffff:ffff\t13335\tUS\tCLOUDFLARENET\n";

    #[test]
    fn looks_up_ranges() {
        let t = parse(SAMPLE);
        assert_eq!(t.lookup("1.0.0.1".parse().unwrap()).unwrap().name, "CLOUDFLARENET");
        assert_eq!(t.lookup("8.8.8.8".parse().unwrap()).unwrap().number, 15169);
        assert_eq!(t.lookup("2606:4700::6810:84e5".parse().unwrap()).unwrap().number, 13335);
        assert!(t.lookup("1.0.2.1".parse().unwrap()).is_none(), "not routed");
        assert!(t.lookup("9.9.9.9".parse().unwrap()).is_none(), "outside every range");
        assert_eq!(t.lookup("::ffff:8.8.8.8".parse().unwrap()).unwrap().number, 15169);
    }
}

/// Debug aid: `UNSKILLED_NETINFO_DIR=… UNSKILLED_IPS="1.2.3.4 …" cargo test -p unskilled-netinfo lookup_ips -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn lookup_ips() {
    let dir = PathBuf::from(std::env::var("UNSKILLED_NETINFO_DIR").unwrap());
    let table = load(&dir).unwrap();
    for ip in std::env::var("UNSKILLED_IPS").unwrap().split_whitespace() {
        let asn = table.lookup(ip.parse().unwrap());
        let edge = classify(asn.as_ref().map(|a| a.number), asn.as_ref().map(|a| a.name.as_str()), &[]);
        println!("{ip:<40} {asn:?} → {edge:?}");
    }
}
