pub trait ManagedResource {
    type Id;

    fn id(&self) -> Self::Id;

    fn is_alive(&self) -> bool;
}
