use regex::Regex;
use std::fmt::Write;
use std::{env::args, error::Error, fmt::Display, fs, sync::LazyLock};

const BAD_FREQ_MATCH: &str = "Failed to get required item in table row";

static FREQ_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b0x[0-9A-Fa-f]+\b|\b\d+\b|\b\w+\b\([^)]*\)|\b\w+\b").unwrap());
static L2_LEVEL_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"L2\((\d+)\)").unwrap());

#[derive(Debug)]
struct Row {
    freq: u32,
    is_pll8: bool,
    l2_level: u8,
    perf_level: usize,
    uv: [u32; 7],
}

impl Display for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hz = self.freq * 1000;

        let uv = self.uv.iter().enumerate().try_fold(String::with_capacity(200), |mut s, (i, uv)| {
            write!(
                s,
                "\topp-microvolt-speed0-pvs{} = <{} {} {}>;{}",
                i,
                uv,
                uv,
                uv,
                if i == self.uv.len() - 1 { "" } else { "\n" }
            )?;

            Ok(s)
        })?;

        write!(
            f,
            "opp-{} {{
\topp-hz = /bits/ 64 <{}>;
{}
\topp-supported-hw = <0x4007>;
\topp-level = <{}>;{}
}};
",
            hz,
            hz,
            uv,
            self.perf_level,
            if self.is_pll8 {
                "\n\t/* give enough time to switch between PLL8 and HFPLL */
\tclock-latency-ns = <244144>;"
            } else {
                Default::default()
            }
        )
    }
}

impl Row {
    pub fn try_parse_and_fixup_level(pvs: u8, dt: &[Row], content: &str) -> Result<Option<Self>, Box<dyn Error>> {
        let mut freq_match = FREQ_REGEX.find_iter(content);

        let use_for_scaling = freq_match.next().ok_or(BAD_FREQ_MATCH)?.as_str().parse::<u8>()? != 0;
        if !use_for_scaling {
            return Ok(None);
        }

        let freq = freq_match.next().ok_or(BAD_FREQ_MATCH)?.as_str().parse()?;
        let is_pll8 = freq_match.next().ok_or(BAD_FREQ_MATCH)?.as_str() == "PLL_8";
        freq_match.next().ok_or(BAD_FREQ_MATCH)?; // PLL src
        freq_match.next().ok_or(BAD_FREQ_MATCH)?; // PLL value
        let l2_level = L2_LEVEL_REGEX
            .captures(freq_match.next().ok_or(BAD_FREQ_MATCH)?.as_str())
            .ok_or("No captures found for L2(...), please fix your kernel")?
            .get(1)
            .ok_or("No value found in L2(...), please fix your kernel")?
            .as_str()
            .parse()?;
        let uv_value = freq_match.next().ok_or(BAD_FREQ_MATCH)?.as_str().parse()?;
        let perf_level = if let Some(row) = dt.iter().find(|row| row.l2_level == l2_level) {
            row.perf_level
        } else if dt.is_empty() {
            1
        } else {
            dt.iter().last().ok_or("Bad last element in vec")?.perf_level + 1
        };

        let mut uv = [0; 7];
        uv[pvs as usize] = uv_value;

        Ok(Some(Self {
            freq,
            is_pll8,
            l2_level,
            perf_level,
            uv,
        }))
    }
}

fn pvs_macro_to_index(ty: &str) -> Result<u8, &'static str> {
    match ty {
        "PVS_SLOW" => Ok(0),
        "PVS_NOMINAL" => Ok(2),
        "PVS_FAST" => Ok(3),
        "PVS_FASTER" => Ok(4),
        _ => Err("Bad PVS type"),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let pvs_regex = Regex::new(r"\[\s*(.*?)\s*\]\[\s*(.*?)\s*\]\s*=\s*\{\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*")?;
    let array_regex = Regex::new(r"static struct acpu_level .* __initdata = \{([\s\S]*?)\};")?;
    let inner_regex = Regex::new(r"\s*\d+,\s*\{\s*[^}]+\s*\},\s*\w+\(\d+\),\s*\d+")?;

    let path = args().nth(1).ok_or("Please specify the C file path")?;
    let content = fs::read_to_string(&path)?;

    let mut dt = Vec::with_capacity(12);

    let pvs_table = pvs_regex.captures_iter(&content).filter_map(|m| {
        let pvs = m.get(1)?;

        // TODO: always assume speed is a number
        if pvs.as_str().parse::<u8>().is_ok() {
            Some((m.get(2)?.as_str(), m.get(3)?.as_str()))
        } else {
            None
        }
    });

    // acpu_freq_tbl array
    for (table_match, (pvs_name, table_name)) in array_regex.find_iter(&content).map(|m| array_regex.captures(m.as_str())).zip(pvs_table) {
        let table = table_match.ok_or("No acpuclk array found, please fix your kernel")?;
        let ty = pvs_name.parse::<u8>().or_else(|_| pvs_macro_to_index(pvs_name))?;
        let inner = table
            .get(1)
            .ok_or(format!("No contents in {table_name} acpuclk table, please fix your kernel"))?
            .as_str();

        // makes sense only if we don't have freqs yet
        if dt.is_empty() {
            // for each row in table
            for row in inner_regex.find_iter(inner) {
                let row = row.as_str();

                if let Some(row) = Row::try_parse_and_fixup_level(ty, &dt, row)? {
                    dt.push(row);
                }
            }
        } else {
            // at this point everything is parsed and we just need to update value
            for row in inner_regex.find_iter(inner) {
                let row = row.as_str();

                let freq = FREQ_REGEX.find_iter(row).nth(1).ok_or(BAD_FREQ_MATCH)?.as_str().parse()?;
                if let Some(item) = dt.iter_mut().find(|row| row.freq == freq) {
                    item.uv[ty as usize] = FREQ_REGEX.find_iter(row).nth(6).ok_or(BAD_FREQ_MATCH)?.as_str().parse()?;
                }
            }
        }
    }

    if dt.len() > 20 {
        Err("Bad item count, if you're sure it's correct output please bump the limit value".into())
    } else {
        for row in dt {
            println!("{row}");
        }

        Ok(())
    }
}
