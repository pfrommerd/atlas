use std::future::Future;

pub trait AsyncIterator {
    type Item;

    // Required method
    async fn next(&mut self) -> Option<Self::Item>;

    // Provided methods
    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, None)
    }

    #[must_use = "iterators do nothing unless iterated over"]
    fn map<B, F>(self, f: F) -> Map<Self, F>
                where Self: Sized,
                      F: FnMut(Self::Item) -> B {
        Map::new(self, f)
    }

    async fn collect<B: FromAsyncIterator<Self::Item>>(self) -> B
                where Self: Sized {
        let fut = <B as FromAsyncIterator<_>>::from_iter(self);
        fut.await
    }
}

pub trait FromAsyncIterator<A>: Sized {
    // Required method
    async fn from_iter<T: IntoAsyncIterator<Item = A>>(iter: T) -> Self;
}

pub trait IntoAsyncIterator {
    type Item;
    type IntoIter: AsyncIterator<Item = Self::Item>;

    // Required method
    async fn into_iter(self) -> Self::IntoIter;
}

impl<I: AsyncIterator> IntoAsyncIterator for I {
    type Item = I::Item;
    type IntoIter = I;

    async fn into_iter(self) -> I {
        self
    }
}

#[cfg(any(feature = "alloc", feature = "std"))]
impl<T> FromAsyncIterator<T> for std::vec::Vec<T> {
    async fn from_iter<I: IntoAsyncIterator<Item = T>>(iter: I) -> std::vec::Vec<T> {
        let mut iter = iter.into_iter().await;
        let mut output = std::vec::Vec::with_capacity(iter.size_hint().1.unwrap_or_default());
        while let Some(item) = iter.next().await {
            output.push(item);
        }
        output
    }
}

#[derive(Debug)]
pub struct Map<I, F> {
    stream: I,
    f: F,
}

impl<I, F> Map<I, F> {
    pub(crate) fn new(stream: I, f: F) -> Self {
        Self { stream, f }
    }
}


impl<I, F, B, Fut> AsyncIterator for Map<I, F> 
        where I: AsyncIterator,
              F: FnMut(I::Item) -> Fut,
              Fut: Future<Output = B> {

    type Item = B;
    async fn next(&mut self) -> Option<Self::Item> {
        let item = self.stream.next().await?;
        let out = (self.f)(item).await;
        Some(out)
    }
}