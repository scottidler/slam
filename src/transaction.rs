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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use eyre::eyre;

    #[test]
    fn test_transaction_new() {
        let transaction = Transaction::new();
        assert_eq!(transaction.rollsbacks.len(), 0);
        assert!(!transaction.committed);
    }

    #[test]
    fn test_add_rollback() {
        let mut transaction = Transaction::new();

        transaction.add_rollback(|| Ok(()));
        assert_eq!(transaction.rollsbacks.len(), 1);

        transaction.add_rollback(|| Ok(()));
        assert_eq!(transaction.rollsbacks.len(), 2);
    }

    #[test]
    fn test_commit() {
        let mut transaction = Transaction::new();

        transaction.add_rollback(|| Ok(()));
        transaction.add_rollback(|| Ok(()));
        assert_eq!(transaction.rollsbacks.len(), 2);
        assert!(!transaction.committed);

        transaction.commit();
        assert_eq!(transaction.rollsbacks.len(), 0);
        assert!(transaction.committed);
    }

    #[test]
    fn test_rollback_successful_actions() {
        let counter = Arc::new(Mutex::new(0));
        let mut transaction = Transaction::new();

        // Add rollback actions that increment the counter
        let counter_clone1 = Arc::clone(&counter);
        transaction.add_rollback(move || {
            let mut count = counter_clone1.lock().unwrap();
            *count += 1;
            Ok(())
        });

        let counter_clone2 = Arc::clone(&counter);
        transaction.add_rollback(move || {
            let mut count = counter_clone2.lock().unwrap();
            *count += 10;
            Ok(())
        });

        transaction.rollback();

        // Actions should be executed in reverse order: 10 first, then 1
        let final_count = *counter.lock().unwrap();
        assert_eq!(final_count, 11);
        assert_eq!(transaction.rollsbacks.len(), 0);
    }

    #[test]
    fn test_rollback_with_failing_actions() {
        let counter = Arc::new(Mutex::new(0));
        let mut transaction = Transaction::new();

        // Add a successful rollback action
        let counter_clone1 = Arc::clone(&counter);
        transaction.add_rollback(move || {
            let mut count = counter_clone1.lock().unwrap();
            *count += 1;
            Ok(())
        });

        // Add a failing rollback action
        transaction.add_rollback(|| {
            Err(eyre!("Rollback failed"))
        });

        // Add another successful rollback action
        let counter_clone2 = Arc::clone(&counter);
        transaction.add_rollback(move || {
            let mut count = counter_clone2.lock().unwrap();
            *count += 10;
            Ok(())
        });

        transaction.rollback();

        // All actions should be attempted, even if some fail
        // Successful actions: 10 + 1 = 11
        let final_count = *counter.lock().unwrap();
        assert_eq!(final_count, 11);
        assert_eq!(transaction.rollsbacks.len(), 0);
    }

    #[test]
    fn test_rollback_empty_transaction() {
        let mut transaction = Transaction::new();

        // Should not panic on empty rollback
        transaction.rollback();
        assert_eq!(transaction.rollsbacks.len(), 0);
    }

    #[test]
    fn test_multiple_rollbacks() {
        let counter = Arc::new(Mutex::new(0));
        let mut transaction = Transaction::new();

        let counter_clone = Arc::clone(&counter);
        transaction.add_rollback(move || {
            let mut count = counter_clone.lock().unwrap();
            *count += 1;
            Ok(())
        });

        // First rollback
        transaction.rollback();
        assert_eq!(*counter.lock().unwrap(), 1);
        assert_eq!(transaction.rollsbacks.len(), 0);

        // Second rollback should do nothing
        transaction.rollback();
        assert_eq!(*counter.lock().unwrap(), 1);
    }

    #[test]
    fn test_commit_after_rollback() {
        let mut transaction = Transaction::new();

        transaction.add_rollback(|| Ok(()));
        transaction.rollback();

        // Commit after rollback should work but do nothing
        transaction.commit();
        assert!(transaction.committed);
        assert_eq!(transaction.rollsbacks.len(), 0);
    }
}
