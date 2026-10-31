
use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;

struct Group {
    hour: [u8; 13],
    endpoint_id: u32,
    errors: u32,
    latencies: Vec<u32>,
}

fn parse_u32(bytes: &[u8]) -> u32 {
    let mut value = 0u32;
    for &b in bytes {
        value = value * 10 + (b - b'0') as u32;
    }
    value
}

fn main() -> io::Result<()> {
    // Defaults are used by the hackathon container.
    // Optional arguments let us test locally on Windows.
    let args: Vec<String> = env::args().collect();
    let data_dir = args.get(1).map(String::as_str).unwrap_or("/data");
    let out_dir = args.get(2).map(String::as_str).unwrap_or("/out");

    let input_path = Path::new(data_dir).join("requests.log");
    let mut input = Vec::new();
    File::open(input_path)?.read_to_end(&mut input)?;

    // Endpoint strings are interned so each request does not store
    // another copy of its endpoint in the group key.
    let mut endpoint_ids: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut endpoints: Vec<Vec<u8>> = Vec::new();

    // Each group is identified by (UTC hour prefix, endpoint ID).
    let mut group_ids: HashMap<([u8; 13], u32), usize> = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();

    for line in input.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }

        let line = line.strip_suffix(b"\r").unwrap_or(line);

        let mut fields = line.split(|&b| b == b' ');
        let timestamp = fields.next().unwrap();
        let _method = fields.next().unwrap();
        let endpoint = fields.next().unwrap();
        let status = parse_u32(fields.next().unwrap());
        let latency = parse_u32(fields.next().unwrap());

        let mut hour = [0u8; 13];
        hour.copy_from_slice(&timestamp[..13]);

        let endpoint_id = match endpoint_ids.get(endpoint) {
            Some(&id) => id,
            None => {
                let id = endpoints.len() as u32;
                let owned = endpoint.to_vec();
                endpoints.push(owned.clone());
                endpoint_ids.insert(owned, id);
                id
            }
        };

        let key = (hour, endpoint_id);
        let group_index = match group_ids.get(&key) {
            Some(&index) => index,
            None => {
                let index = groups.len();
                groups.push(Group {
                    hour,
                    endpoint_id,
                    errors: 0,
                    latencies: Vec::new(),
                });
                group_ids.insert(key, index);
                index
            }
        };

        let group = &mut groups[group_index];
        group.latencies.push(latency);

        if (500..=599).contains(&status) {
            group.errors += 1;
        }
    }

    // Sort group indices by hour, then by the endpoint's exact byte order.
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_unstable_by(|&a, &b| {
        groups[a]
            .hour
            .cmp(&groups[b].hour)
            .then_with(|| {
                endpoints[groups[a].endpoint_id as usize]
                    .cmp(&endpoints[groups[b].endpoint_id as usize])
            })
    });

    fs::create_dir_all(out_dir)?;
    let output_path = Path::new(out_dir).join("summary.txt");
    let mut output = BufWriter::new(File::create(output_path)?);

    for index in order {
        let group = &mut groups[index];
        group.latencies.sort_unstable();

        let count = group.latencies.len();
        let p50_rank = (50 * count + 99) / 100 - 1;
        let p95_rank = (95 * count + 99) / 100 - 1;
        let p99_rank = (99 * count + 99) / 100 - 1;

        let p50 = group.latencies[p50_rank];
        let p95 = group.latencies[p95_rank];
        let p99 = group.latencies[p99_rank];

        let error_ppm =
            (group.errors as u64 * 1_000_000) / count as u64;

        output.write_all(&group.hour)?;
        output.write_all(b":00:00Z ")?;
        output.write_all(&endpoints[group.endpoint_id as usize])?;

        writeln!(
            output,
            " {} {} {} {} {} {}",
            count, p50, p95, p99, group.errors, error_ppm
        )?;
    }

    output.flush()?;
    Ok(())
}
