//! Paged listings (bookings, document lists). A failing first page fails the
//! listing; a later one ends it early, and the caller decides whether part of
//! a listing may be stored.

use std::future::Future;

use crate::error::{CoreError, CoreResult};

/// What a paged listing read.
#[derive(Debug)]
pub(crate) struct Paged<T> {
    pub items: Vec<T>,
    /// Why the listing ended before its last page (a page failed, too many
    /// pages); `None` when every page was read.
    pub stopped: Option<CoreError>,
}

impl<T> Paged<T> {
    /// The items of a listing that was read to its end, else the error that
    /// stopped it. For callers that must not store part of a listing: the
    /// next sync starts after the newest stored booking, so bookings behind
    /// a failed page could be skipped for good.
    pub fn complete(self) -> CoreResult<Vec<T>> {
        match self.stopped {
            Some(e) => Err(e),
            None => Ok(self.items),
        }
    }
}

/// Reads at most `max_pages` pages, starting at cursor `first`. `page` returns
/// one page's items and the cursor of the next page (`None` after the last).
pub(crate) async fn paginate<C, T, F>(
    max_pages: usize,
    first: C,
    mut page: impl FnMut(C) -> F,
) -> CoreResult<Paged<T>>
where
    F: Future<Output = CoreResult<(Vec<T>, Option<C>)>>,
{
    let mut items = Vec::new();
    let mut cursor = first;
    for n in 0..max_pages {
        let (got, next) = match page(cursor).await {
            Ok(page) => page,
            Err(e) if n == 0 => return Err(e),
            Err(e) => {
                return Ok(Paged {
                    items,
                    stopped: Some(e),
                });
            }
        };
        items.extend(got);
        let Some(next) = next else {
            return Ok(Paged {
                items,
                stopped: None,
            });
        };
        cursor = next;
    }
    Ok(Paged {
        items,
        stopped: Some(CoreError::protocol(format!(
            "listing has more than {max_pages} pages"
        ))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pages of two items each, numbered from 0; `fail` makes that page fail.
    async fn listing(
        pages: usize,
        max_pages: usize,
        fail: Option<usize>,
    ) -> CoreResult<Paged<usize>> {
        paginate(max_pages, 0, |n: usize| async move {
            if fail == Some(n) {
                return Err(CoreError::protocol("page failed"));
            }
            let next = (n + 1 < pages).then_some(n + 1);
            Ok((vec![2 * n, 2 * n + 1], next))
        })
        .await
    }

    #[tokio::test]
    async fn reads_every_page() {
        let p = listing(3, 10, None).await.unwrap();
        assert!(p.stopped.is_none());
        assert_eq!(p.complete().unwrap(), vec![0, 1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn a_failing_first_page_fails_the_listing() {
        assert!(listing(3, 10, Some(0)).await.is_err());
    }

    #[tokio::test]
    async fn a_later_failing_page_keeps_the_earlier_ones() {
        let p = listing(3, 10, Some(1)).await.unwrap();
        assert_eq!(p.items, vec![0, 1]);
        assert!(p.stopped.is_some());
        assert!(listing(3, 10, Some(1)).await.unwrap().complete().is_err());
    }

    #[tokio::test]
    async fn too_many_pages_is_incomplete() {
        let p = listing(5, 2, None).await.unwrap();
        assert_eq!(p.items, vec![0, 1, 2, 3]);
        assert!(p.stopped.is_some());
        // exactly `max_pages` pages is complete
        assert!(listing(2, 2, None).await.unwrap().stopped.is_none());
    }
}
