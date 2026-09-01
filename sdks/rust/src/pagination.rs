use crate::error::SynapseError;
use futures_core::Stream;
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

/// Turn a [`PageIter`]-style page-fetch closure into a `Stream` that yields
/// individual items, transparently fetching the next page once the current
/// page is exhausted and the caller keeps polling.
///
/// Only one page is ever buffered in memory. A page-fetch error is yielded
/// as a single `Err` item and ends the stream, mirroring [`PageIter`]. Since
/// `fetch` is expected to be backed by [`crate::client::SynapseClient`]
/// (which already retries transient failures with backoff via
/// [`crate::retry::retry_with_backoff`]), callers get rate-limit-aware
/// pagination for free without adding a second retry layer here.
///
/// # Example
///
/// ```no_run
/// # use synapse_sdk::pagination::auto_follow;
/// # use synapse_sdk::error::SynapseError;
/// # async fn example() -> Result<(), SynapseError> {
/// let _stream = auto_follow(|cursor: Option<String>| async move {
///     let items: Vec<String> = vec![];
///     let next_cursor: Option<String> = None;
///     Ok::<_, SynapseError>((items, next_cursor))
/// });
/// // Consume with any `Stream` combinator, e.g. `futures_util::StreamExt::next`.
/// # Ok(())
/// # }
/// ```
pub fn auto_follow<T, F, Fut>(fetch: F) -> impl Stream<Item = Result<T, SynapseError>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<(Vec<T>, Option<String>), SynapseError>>,
{
    async_stream::stream! {
        let mut iter = PageIter::new(fetch);
        while let Some(page) = iter.next_page().await {
            match page {
                Ok(items) => {
                    for item in items {
                        yield Ok(item);
                    }
                }
                Err(err) => {
                    yield Err(err);
                    break;
                }
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

    // ── auto_follow ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn auto_follow_yields_every_item_across_pages_in_order() {
        use futures_util::StreamExt;

        let pages = Arc::new(Mutex::new(vec![
            (vec![1u32, 2], Some("c1".to_string())),
            (vec![3u32, 4], Some("c2".to_string())),
            (vec![5u32], None::<String>),
        ]));

        let stream = auto_follow(move |_cursor| {
            let pages = pages.clone();
            async move {
                let entry = {
                    let mut lock = pages.lock().unwrap();
                    lock.remove(0)
                };
                Ok::<_, SynapseError>(entry)
            }
        });

        let items: Vec<Result<u32, SynapseError>> = stream.collect().await;
        let items: Vec<u32> = items.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(items, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn auto_follow_terminates_cleanly_on_last_page() {
        use futures_util::StreamExt;

        let stream =
            auto_follow(
                |_cursor| async move { Ok::<_, SynapseError>((vec![1u32], None::<String>)) },
            );

        let items: Vec<Result<u32, SynapseError>> = stream.collect().await;
        assert_eq!(items.len(), 1);
        assert_eq!(*items[0].as_ref().unwrap(), 1);
    }

    #[tokio::test]
    async fn auto_follow_propagates_error_mid_stream_and_stops() {
        use futures_util::StreamExt;

        let pages: Arc<Mutex<Vec<Result<(Vec<u32>, Option<String>), SynapseError>>>> =
            Arc::new(Mutex::new(vec![
                Ok((vec![1, 2], Some("c1".to_string()))),
                Err(SynapseError::Http {
                    status: 500,
                    body: "boom".to_string(),
                }),
            ]));

        let stream = auto_follow(move |_cursor| {
            let pages = pages.clone();
            async move {
                let mut lock = pages.lock().unwrap();
                lock.remove(0)
            }
        });

        let items: Vec<Result<u32, SynapseError>> = stream.collect().await;
        assert_eq!(items.len(), 3, "2 items from page 1, then 1 error item");
        assert!(matches!(items[0], Ok(1)));
        assert!(matches!(items[1], Ok(2)));
        assert!(
            matches!(items[2], Err(SynapseError::Http { status: 500, .. })),
            "the error must be surfaced as the final stream item"
        );
    }
}
