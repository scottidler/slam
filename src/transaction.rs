use log::{debug, error};
use eyre::Result;
//------------------------------------------------------------------------------
// Transaction Struct Definition
//------------------------------------------------------------------------------
/// A single rollback action: a boxed function that undoes a previously completed step.
type Rollback = Box<dyn Fn() -> Result<()> + Send>;

/// Transaction is a rollback stack for reversible operations.
/// Each successful step can register a rollback closure. On error, all actions
/// are invoked in reverse order.
pub struct Transaction {
    rollsbacks: Vec<Rollback>,
    committed: bool,
}

impl Transaction {
    pub fn new() -> Self {
        Transaction {
            rollsbacks: Vec::new(),
            committed: false,
        }
    }

    /// Registers a new rollback action.
    pub fn add_rollback<F>(&mut self, action: F)
    where
        F: Fn() -> Result<()> + Send + 'static,
    {
        self.rollsbacks.push(Box::new(action));
    }

    /// Executes rollback actions in reverse order. Each error is logged.
    pub fn rollback(&mut self) {
        error!("An error occurred; initiating rollback of {} actions", self.rollsbacks.len());
        while let Some(action) = self.rollsbacks.pop() {
            if let Err(e) = action() {
                error!("Rollback action failed: {:?}", e);
            } else {
                debug!("Rollback action succeeded");
            }
        }
    }

    /// Marks the transaction as committed and clears the rollback stack.
    pub fn commit(&mut self) {
        self.committed = true;
        self.rollsbacks.clear();
    }
}

