#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ImageCase {
    pub(crate) prompt: String,
    pub(crate) negative_prompt: String,
    pub(crate) seed: u32,
}

pub(crate) fn selected() -> anyhow::Result<(usize, ImageCase)> {
    let index = std::env::var("LUMINA_SD15_ACCEPTANCE_CASE")
        .unwrap_or_else(|_| "1".into())
        .parse::<usize>()?;
    let rows: Vec<ImageCase> = serde_json::from_str(include_str!("sd15-cases.json"))?;
    Ok((
        index,
        rows.into_iter()
            .nth(
                index
                    .checked_sub(1)
                    .ok_or_else(|| anyhow::anyhow!("Unknown image case"))?,
            )
            .ok_or_else(|| anyhow::anyhow!("Unknown image case"))?,
    ))
}
