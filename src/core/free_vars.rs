use std::collections::HashSet;
pub trait FreeVariables {
    fn free_variables<'e>(&'e self, bound: HashSet<&str>) -> HashSet<&'e str>;
}