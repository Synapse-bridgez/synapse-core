use crate::error::SynapseError;
use std::future::Future;
use std::marker::PhantomData;

/// A lazy, page-by-page iterator over a cursor-paginated endpoint.
///
/// Each call to [`PageIter::next_page`] issues exactly one network request.
/// The iterator is exhausted when the server returns `next_cursor: None`; all
/// subsequent calls return `None` without making further requests.
///
/// Build one from any async closure that accepts `Option<String>` (the cursor)
/// and returns `Result<(Vec<T>, Option<String>), SynapseError>`:
///
/// ```no_run
/// # use synapse_sdk::pagination::PageIter;
/// # use synapse_sdk::error::SynapseError;
/// # async fn example() -> Result<(), SynapseError> {
/// let mut iter = PageIter::new(|cursor| async move {
///     // replace with a real client.transactions.list(cursor, limit) call
///     let items: Vec<String> = vec![];
///     let next_cursor: Option<String> = None;
///     Ok((items, next_cursor))
/// });
///
/// while let Some(page) = iter.next_page().await {
///     for item in page? {
///         println!("{item}");
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct PageIter<T, F, Fut> {
    fetch: F,
    cursor: Option<String>,
    done: bool,
    _marker: PhantomData<fn() -> (T, Fut)>,
}

impl<T, F, Fut> PageIter<T, F, Fut>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<(Vec<T>, Option<String>), SynapseError>>,
{
    /// Create a new iterator backed by `fetch`.
    ///
    /// `fetch` is called with the current cursor on each page request. Return
    /// `(items, Some(cursor))` to indicate more pages or `(items, None)` to
    /// signal the last page.
    pub fn new(fetch: F) -> Self {
        Self {
            fetch,
            cursor: None,
            done: false,
            _marker: PhantomData,
        }
    }

    /// Fetch the next page.
    ///
    /// Returns `None` once the server has signalled that no more pages exist.
    /// If the underlying request fails, the iterator is marked done and the
    /// error is surfaced as `Some(Err(...))` so the caller can handle it; any
    /// further call returns `None`.
    pub async fn next_page(&mut self) -> Option<Result<Vec<T>, SynapseError>> {
        if self.done {
            return None;
        }
        let cursor = self.cursor.take();
        match (self.fetch)(cursor).await {
            Ok((items, next_cursor)) => {
                match next_cursor {
                    Some(c) => self.cursor = Some(c),
                    None => self.done = true,
                }
                Some(Ok(items))
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn collects_all_pages_and_stops() {
        let pages = Arc::new(Mutex::new(vec![
            (vec![1u32, 2], Some("c1".to_string())),
            (vec![3u32, 4], Some("c2".to_string())),
            (vec![5u32], None::<String>),
        ]));

        let mut iter = PageIter::new(|_cursor| {
            let pages = pages.clone();
            async move {
                let entry = {
                    let mut lock = pages.lock().unwrap();
                    lock.remove(0)
                };
                Ok::<_, SynapseError>(entry)
            }
        });

        let mut all = Vec::new();
        while let Some(page) = iter.next_page().await {
            all.extend(page.unwrap());
        }
        assert_eq!(all, vec![1, 2, 3, 4, 5]);
        // Exhausted iterator must keep returning None.
        assert!(iter.next_page().await.is_none());
    }

    #[tokio::test]
    async fn passes_cursor_to_each_fetch() {
        let cursors_seen: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));

        let responses: Arc<Mutex<Vec<(Vec<u8>, Option<String>)>>> = Arc::new(Mutex::new(vec![
            (vec![1], Some("tok".to_string())),
            (vec![2], None),
        ]));

        let mut iter = PageIter::new(|cursor| {
            let seen = cursors_seen.clone();
            let responses = responses.clone();
            async move {
                seen.lock().unwrap().push(cursor);
                let entry = {
                    let mut lock = responses.lock().unwrap();
                    lock.remove(0)
                };
                Ok::<_, SynapseError>(entry)
            }
        });

        while let Some(page) = iter.next_page().await {
            page.unwrap();
        }

        let seen = cursors_seen.lock().unwrap();
        assert_eq!(seen[0], None, "first call must pass None");
        assert_eq!(
            seen[1],
            Some("tok".to_string()),
            "second call must pass the cursor from page 1"
        );
    }

    #[tokio::test]
    async fn surfaces_error_and_stops() {
        let mut iter = PageIter::<u32, _, _>::new(|_cursor| async move {
            Err::<(Vec<u32>, Option<String>), _>(SynapseError::Http {
                status: 500,
                body: "oops".to_string(),
            })
        });

        let result = iter.next_page().await;
        assert!(
            matches!(result, Some(Err(SynapseError::Http { status: 500, .. }))),
            "error should be surfaced"
        );
        assert!(
            iter.next_page().await.is_none(),
            "iterator must stop after an error"
        );
    }
}
