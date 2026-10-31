use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

const HOURS: usize = 72;
const ENDPOINTS: usize = 1_842;
const GROUPS: usize = HOURS * ENDPOINTS;
const INPUT_LINES: usize = 3_100_000;

#[derive(Clone, Copy, Default)]
struct Stats {
    count: u32,
    errors: u32,
    p50: u32,
    p95: u32,
    p99: u32,
}

fn parse_u32(bytes: &[u8]) -> u32 {
    let mut value = 0u32;
    for &b in bytes {
        value = value * 10 + (b - b'0') as u32;
    }
    value
}

// The specification fixes the input span to these 72 UTC hours.
fn hour_slot(timestamp: &[u8]) -> usize {
    let month = parse_u32(&timestamp[5..7]);
    let day = parse_u32(&timestamp[8..10]);
    let hour = parse_u32(&timestamp[11..13]);

    match (month, day) {
        (10, 30) => (hour - 18) as usize,
        (10, 31) => (6 + hour) as usize,
        (11, 1) => (30 + hour) as usize,
        (11, 2) => (54 + hour) as usize,
        _ => unreachable!("timestamp outside the specified 72-hour span"),
    }
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let data_dir = args.get(1).map(String::as_str).unwrap_or("/data");
    let out_dir = args.get(2).map(String::as_str).unwrap_or("/out");

    let input_path = Path::new(data_dir).join("requests.log");
    let mut reader = BufReader::with_capacity(1 << 20, File::open(input_path)?);
    let mut line = Vec::with_capacity(160);

    // Endpoint IDs are compact; group IDs are dense: hour * 1842 + endpoint.
    let mut endpoint_ids: HashMap<Vec<u8>, u16> = HashMap::with_capacity(2_048);
    let mut endpoints: Vec<Vec<u8>> = Vec::with_capacity(ENDPOINTS);
    let mut stats = vec![Stats::default(); GROUPS];

    // Each u64 packs (group_id << 32) | latency.
    // Sorting these values orders groups and their latencies in one operation.
    let mut values: Vec<u64> = Vec::with_capacity(INPUT_LINES);

    while reader.read_until(b'\n', &mut line)? != 0 {
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            line.clear();
            continue;
        }

        // Locate the four spaces without allocating field strings.
        let s1 = line.iter().position(|&b| b == b' ').unwrap();
        let s2 = s1 + 1 + line[s1 + 1..].iter().position(|&b| b == b' ').unwrap();
        let s3 = s2 + 1 + line[s2 + 1..].iter().position(|&b| b == b' ').unwrap();
        let s4 = s3 + 1 + line[s3 + 1..].iter().position(|&b| b == b' ').unwrap();

        let timestamp = &line[..s1];
        let endpoint = &line[s2 + 1..s3];
        let status = parse_u32(&line[s3 + 1..s4]);
        let latency = parse_u32(&line[s4 + 1..]);

        let hour = hour_slot(timestamp);

        let endpoint_id = match endpoint_ids.get(endpoint) {
            Some(&id) => id as usize,
            None => {
                let id = endpoints.len() as u16;
                let owned = endpoint.to_vec();
                endpoints.push(owned.clone());
                endpoint_ids.insert(owned, id);
                id as usize
            }
        };

        let group_id = hour * ENDPOINTS + endpoint_id;
        let group = &mut stats[group_id];
        group.count += 1;
        if (500..=599).contains(&status) {
            group.errors += 1;
        }

        values.push(((group_id as u64) << 32) | latency as u64);
        line.clear();
    }

    // Numeric sort: group ID first, then latency (both ascending).
    values.sort_unstable();

    // Read exact nearest-rank percentiles from each group's sorted segment.
    let mut cursor = 0usize;
    for group in &mut stats {
        let count = group.count as usize;
        if count == 0 {
            continue;
        }

        let p50_rank = (50 * count + 99) / 100 - 1;
        let p95_rank = (95 * count + 99) / 100 - 1;
        let p99_rank = (99 * count + 99) / 100 - 1;

        group.p50 = values[cursor + p50_rank] as u32;
        group.p95 = values[cursor + p95_rank] as u32;
        group.p99 = values[cursor + p99_rank] as u32;
        cursor += count;
    }
    debug_assert_eq!(cursor, values.len());

    // Endpoint IDs were assigned by first appearance, not byte order.
    // Sort the IDs once, then emit hours chronologically and endpoints bytewise.
    let mut endpoint_order: Vec<usize> = (0..endpoints.len()).collect();
    endpoint_order.sort_unstable_by(|&a, &b| endpoints[a].cmp(&endpoints[b]));

    fs::create_dir_all(out_dir)?;
    let output_path = Path::new(out_dir).join("summary.txt");
    let mut output = BufWriter::with_capacity(1 << 20, File::create(output_path)?);

    // The hour label is generated from the fixed 72-hour span.
    let mut hour_labels = [[0u8; 13]; HOURS];
    for slot in 0..HOURS {
        let (month, day, hour) = match slot {
            0..=5 => (10u32, 30u32, 18u32 + slot as u32),
            6..=29 => (10, 31, (slot - 6) as u32),
            30..=53 => (11, 1, (slot - 30) as u32),
            _ => (11, 2, (slot - 54) as u32),
        };
        let label = format!("2026-{month:02}-{day:02}T{hour:02}");
        hour_labels[slot][..13].copy_from_slice(label.as_bytes());
    }

    for hour in 0..HOURS {
        for &endpoint_id in &endpoint_order {
            let group = stats[hour * ENDPOINTS + endpoint_id];
            if group.count == 0 {
                continue;
            }

            output.write_all(&hour_labels[hour])?;
            output.write_all(b":00:00Z ")?;
            output.write_all(&endpoints[endpoint_id])?;

            let error_ppm =
                (group.errors as u64 * 1_000_000) / group.count as u64;

            writeln!(
                output,
                " {} {} {} {} {} {}",
                group.count,
                group.p50,
                group.p95,
                group.p99,
                group.errors,
                error_ppm
            )?;
        }
    }

    output.flush()?;
    Ok(())
}
