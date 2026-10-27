//! Byte-preserving authored-message regions and Redmine references.

const KEYWORDS: [&[u8]; 6] = [
    b"refs",
    b"references",
    b"ref",
    b"fixes",
    b"closes",
    b"closed",
];
const SCISSORS: &[u8] = b" ------------------------ >8 ------------------------";

pub struct Message<'a> {
    bytes: &'a [u8],
    marker: &'a [u8],
    end: usize,
}

fn lines(bytes: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let mut offset = 0;
    bytes.split_inclusive(|b| *b == b'\n').map(move |line| {
        let start = offset;
        offset += line.len();
        let content = if let Some(content) = line.strip_suffix(b"\n") {
            content.strip_suffix(b"\r").unwrap_or(content)
        } else {
            line
        };
        (start, content)
    })
}

impl<'a> Message<'a> {
    pub fn new(bytes: &'a [u8], marker: &'a [u8]) -> Self {
        let end = lines(bytes)
            .find(|(_, line)| line.strip_prefix(marker) == Some(SCISSORS))
            .map_or(bytes.len(), |(offset, _)| offset);
        Self { bytes, marker, end }
    }

    fn authored(&self) -> impl Iterator<Item = &[u8]> {
        lines(&self.bytes[..self.end])
            .map(|(_, line)| line)
            .filter(|line| !line.starts_with(self.marker))
    }

    pub fn references(&self) -> Vec<u64> {
        let mut ids = Vec::new();
        for line in self.authored() {
            for at in 0..line.len() {
                if at > 0 && word(line[at - 1]) {
                    continue;
                }
                for keyword in KEYWORDS {
                    if !line
                        .get(at..at + keyword.len())
                        .is_some_and(|s| s.eq_ignore_ascii_case(keyword))
                    {
                        continue;
                    }
                    let mut cursor = at + keyword.len();
                    let colon = line.get(cursor) == Some(&b':');
                    if colon {
                        cursor += 1;
                    }
                    let before_space = cursor;
                    while line.get(cursor).is_some_and(|b| matches!(b, b' ' | b'\t')) {
                        cursor += 1;
                    }
                    if (!colon && cursor == before_space) || line.get(cursor) != Some(&b'#') {
                        continue;
                    }
                    cursor += 1;
                    let start = cursor;
                    let mut id = Some(0u64);
                    while let Some(b) = line.get(cursor).filter(|b| b.is_ascii_digit()) {
                        id = id.and_then(|v| v.checked_mul(10)?.checked_add(u64::from(*b - b'0')));
                        cursor += 1;
                    }
                    if cursor == start || line.get(cursor).is_some_and(|b| word(*b)) {
                        continue;
                    }
                    if let Some(id) = id.filter(|id| *id > 0)
                        && !ids.contains(&id)
                    {
                        ids.push(id);
                    }
                }
            }
        }
        ids
    }

    pub fn generated_count(&self, id: u64) -> usize {
        let generated = format!("Refs #{id}");
        self.authored()
            .filter(|line| line.trim_ascii() == generated.as_bytes())
            .count()
    }

    /// Append one `Refs #N` line per id, in the given order, as a single
    /// trailer block. Byte preservation and EOL selection match the
    /// historical single-id insertion exactly.
    pub fn insert_many(&self, ids: &[u64]) -> Vec<u8> {
        if ids.is_empty() {
            return self.bytes.to_vec();
        }
        let eol = predominant_eol(self.bytes);
        let mut end = self.end;
        // Remove complete line terminators only; an arbitrary trailing CR is data.
        while end > 0 && self.bytes[end - 1] == b'\n' {
            end -= 1;
            if end > 0 && self.bytes[end - 1] == b'\r' {
                end -= 1;
            }
        }
        let mut result = self.bytes[..end].to_vec();
        result.extend_from_slice(eol);
        result.extend_from_slice(eol);
        for id in ids {
            result.extend_from_slice(format!("Refs #{id}").as_bytes());
            result.extend_from_slice(eol);
        }
        result.extend_from_slice(&self.bytes[self.end..]);
        result
    }
}

fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn predominant_eol(bytes: &[u8]) -> &'static [u8] {
    let mut crlf = 0;
    let mut lf = 0;
    let mut first = None;
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'\n' {
            continue;
        }
        let is_crlf = i > 0 && bytes[i - 1] == b'\r';
        first.get_or_insert(is_crlf);
        if is_crlf {
            crlf += 1;
        } else {
            lf += 1;
        }
    }
    if crlf > lf || (crlf == lf && first == Some(true)) {
        b"\r\n"
    } else {
        b"\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn references_respect_regions_and_boundaries() {
        let bytes = b"Xrefs #1 refs #2abc refs #3_suffix _refs #4 fixes:#9\n; refs #10\n refs #11\n; ------------------------ >8 ------------------------\nrefs #12\n";
        assert_eq!(Message::new(bytes, b";").references(), vec![9, 11]);
        assert!(
            Message::new(b"refs::#9 refs#9 refs #0", b"#")
                .references()
                .is_empty()
        );
    }
    #[test]
    fn insertion_preserves_bytes_signoff_and_scissors() {
        let bytes = b"subject\xff\r\n\r\nSigned-off-by: A <a@b>\r\n\r\n# ------------------------ >8 ------------------------\r\nrefs #99\r\n";
        let out = Message::new(bytes, b"#").insert_many(&[9]);
        assert_eq!(out, b"subject\xff\r\n\r\nSigned-off-by: A <a@b>\r\n\r\nRefs #9\r\n# ------------------------ >8 ------------------------\r\nrefs #99\r\n");
        assert_eq!(Message::new(&out, b"#").references(), vec![9]);
    }
    #[test]
    fn multiple_refs_share_one_trailer_block() {
        let bytes = b"subject\xff\r\n\r\n# ------------------------ >8 ------------------------\r\nrefs #99\r\n";
        let out = Message::new(bytes, b"#").insert_many(&[9, 10]);
        assert_eq!(
            out,
            b"subject\xff\r\n\r\nRefs #9\r\nRefs #10\r\n# ------------------------ >8 ------------------------\r\nrefs #99\r\n"
        );
        assert_eq!(Message::new(&out, b"#").references(), vec![9, 10]);
    }

    #[test]
    fn marker_and_eol_are_exact() {
        assert!(
            Message::new(b";; refs #8\n", b";; ")
                .references()
                .is_empty()
        );
        assert_eq!(Message::new(b" refs #8\n", b"#").references(), vec![8]);
        assert_eq!(predominant_eol(b"a\r\nb\n"), b"\r\n");
        assert_eq!(
            Message::new(b"a\r", b"#").insert_many(&[9]),
            b"a\r\n\nRefs #9\n"
        );
    }
}
