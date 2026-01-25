fn normalize_token(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0; m + 1];

    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        prev.clone_from_slice(&curr);
    }
    prev[m]
}

fn score_candidate(input: &str, candidate: &str) -> usize {
    let a = normalize_token(input);
    let b = normalize_token(candidate);
    if a.is_empty() || b.is_empty() {
        return usize::MAX;
    }
    if a == b {
        return 0;
    }
    if a.contains(&b) || b.contains(&a) {
        return 1;
    }
    levenshtein(&a, &b)
}

fn max_allowed_distance(input: &str) -> usize {
    let normalized = normalize_token(input);
    if normalized.is_empty() {
        return 0;
    }
    if normalized.len() <= 4 {
        return 1;
    }
    if normalized.len() <= 8 {
        return 2;
    }
    (normalized.len() as f32 * 0.35).floor().max(3.0) as usize
}

pub fn suggest(input: &str, candidates: &[String], limit: usize) -> Vec<String> {
    if input.trim().is_empty() || candidates.is_empty() {
        return Vec::new();
    }
    let limit = limit.max(1);
    let cap = candidates.len().min(2000);
    let allowed = max_allowed_distance(input);

    let mut scored: Vec<(String, usize)> = Vec::new();
    for candidate in candidates.iter().take(cap) {
        let score = score_candidate(input, candidate);
        if score <= allowed {
            scored.push((candidate.clone(), score));
        }
    }

    scored.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| a.0.len().cmp(&b.0.len()))
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut out = Vec::new();
    for (cand, _) in scored {
        if out.contains(&cand) {
            continue;
        }
        out.push(cand);
        if out.len() >= limit {
            break;
        }
    }
    out
}
