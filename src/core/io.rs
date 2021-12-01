use bytes::Bytes;

trait Resource {
    fn parent(&self) -> Option<Box<dyn Resource>>;
    fn data(&self) -> Bytes;
}
