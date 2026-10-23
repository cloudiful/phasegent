use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageRequest {
    page: usize,
    limit: usize,
    offset: usize,
}

impl PageRequest {
    pub(crate) fn new(page: usize, limit: usize) -> Result<Self, String> {
        if page == 0 {
            return Err("--page must be greater than zero".to_owned());
        }
        if !(1..=100).contains(&limit) {
            return Err("--limit must be between 1 and 100 for pagination".to_owned());
        }
        let offset = (page - 1)
            .checked_mul(limit)
            .filter(|offset| i64::try_from(*offset).is_ok())
            .ok_or_else(|| "pagination offset is too large".to_owned())?;
        Ok(Self {
            page,
            limit,
            offset,
        })
    }
    pub(crate) fn page(self) -> usize {
        self.page
    }
    pub(crate) fn limit(self) -> usize {
        self.limit
    }
    pub(crate) fn offset(self) -> usize {
        self.offset
    }
}

pub(crate) fn truncation_warning(total: usize, shown: usize) -> Option<String> {
    (total > shown).then(|| format!("showing {shown} of {total} entries; use --page and --limit to read the remaining entries"))
}

#[derive(Debug, Serialize)]
pub(crate) struct Paged<T> {
    pub items: Vec<T>,
    pub page: usize,
    pub limit: usize,
    pub total_count: Option<usize>,
    pub has_more: bool,
}

impl<T: Serialize> Paged<T> {
    pub(crate) fn known(items: Vec<T>, request: PageRequest, total_count: usize) -> Self {
        let has_more = request.offset().saturating_add(items.len()) < total_count;
        Self {
            items,
            page: request.page(),
            limit: request.limit(),
            total_count: Some(total_count),
            has_more,
        }
    }

    pub(crate) fn with_context(
        self,
        context: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let mut value = serde_json::to_value(self).map_err(|error| error.to_string())?;
        let object = context
            .as_object()
            .ok_or_else(|| "pagination context must be an object".to_owned())?;
        let target = value
            .as_object_mut()
            .expect("paged serialization is an object");
        for (key, value) in object {
            if target.contains_key(key) {
                return Err(format!("reserved pagination context key: {key}"));
            }
            target.insert(key.clone(), value.clone());
        }
        Ok(value)
    }
}

pub(crate) fn slice<T: Serialize>(items: Vec<T>, request: PageRequest) -> Paged<T> {
    let total = items.len();
    Paged::known(
        items
            .into_iter()
            .skip(request.offset())
            .take(request.limit())
            .collect(),
        request,
        total,
    )
}

pub(crate) fn native_page<T, E>(
    request: PageRequest,
    mut fetch: impl FnMut(usize, usize) -> Result<(Vec<T>, Option<usize>), E>,
    invalid: impl Fn(&str) -> E,
) -> Result<Paged<T>, E> {
    let (items, total_count) = fetch(request.offset(), request.limit())?;
    if items.len() > request.limit() {
        return Err(invalid("upstream exceeded the requested page limit"));
    }
    let next = request
        .offset()
        .checked_add(items.len())
        .ok_or_else(|| invalid("page offset overflow"))?;
    let has_more = match total_count {
        Some(total) => {
            let more = next < total;
            if more && items.len() < request.limit() {
                return Err(invalid(
                    "upstream returned an incomplete page before its total",
                ));
            }
            more
        }
        None => {
            let (lookahead, _) = fetch(next, 1)?;
            if lookahead.len() > 1 {
                return Err(invalid("upstream exceeded the lookahead limit"));
            }
            if !lookahead.is_empty() && items.len() < request.limit() {
                return Err(invalid(
                    "upstream returned an incomplete page without a total",
                ));
            }
            !lookahead.is_empty()
        }
    };
    Ok(Paged {
        items,
        page: request.page(),
        limit: request.limit(),
        total_count,
        has_more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_page_never_guesses_more_from_an_incomplete_or_failed_response() {
        let request = PageRequest::new(1, 2).unwrap();
        let result = native_page(request, |_, _| Ok((vec![1], Some(3))), str::to_owned);
        assert!(result.unwrap_err().contains("incomplete"));
        let mut calls = 0;
        let result = native_page(
            request,
            |_, _| {
                calls += 1;
                if calls == 1 {
                    Ok((vec![1, 2], None))
                } else {
                    Err("unavailable lookahead".to_owned())
                }
            },
            str::to_owned,
        );
        assert_eq!(result.unwrap_err(), "unavailable lookahead");
        assert_eq!(calls, 2);
    }

    #[test]
    fn bounds_and_context_are_checked() {
        for (page, limit) in [(0, 50), (1, 0), (1, 101), (usize::MAX, 100)] {
            assert!(PageRequest::new(page, limit).is_err());
        }
        let request = PageRequest::new(2, 50).unwrap();
        assert_eq!(request.offset(), 50);
        assert!(
            Paged::known(vec![1], request, 51)
                .with_context(serde_json::json!({"items": []}))
                .is_err()
        );
        assert!(!Paged::<u8>::known(Vec::new(), request, 2).has_more);
    }
}
