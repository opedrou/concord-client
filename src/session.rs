//! Cookie de sessão no cofre do sistema, uma entrada por servidor: cookie de um
//! servidor nunca vai pra outro. Chamadas bloqueiam, então rodam em `spawn_blocking`.
use keyring::Entry;
use tokio::task::spawn_blocking;

use crate::api::Secret;

const SERVICO: &str = "concord";

pub async fn load(server: String) -> Option<Secret> {
    spawn_blocking(move || {
        Entry::new(SERVICO, &server)
            .ok()?
            .get_password()
            .ok()
            .map(Secret)
    })
    .await
    .ok()
    .flatten()
}

/// `false`: cofre indisponível. O app segue, e pede login na próxima abertura.
pub async fn save(server: String, cookie: Secret) -> bool {
    spawn_blocking(move || {
        Entry::new(SERVICO, &server)
            .and_then(|e| e.set_password(&cookie.0))
            .is_ok()
    })
    .await
    .unwrap_or(false)
}

pub async fn delete(server: String) {
    let _ = spawn_blocking(move || delete_blocking(&server)).await;
}

/// Sem async, pro `--uninstall`, que roda antes de existir runtime.
pub fn delete_blocking(server: &str) {
    if let Ok(e) = Entry::new(SERVICO, server) {
        let _ = e.delete_credential();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Precisa de cofre de verdade: `cargo test -- --ignored cofre`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn cofre_guarda_le_e_apaga() {
        let s = "https://teste-do-cliente.invalid".to_string();
        assert!(save(s.clone(), Secret("session=abc".into())).await);
        assert_eq!(load(s.clone()).await, Some(Secret("session=abc".into())));
        delete(s.clone()).await;
        assert_eq!(load(s).await, None);
    }
}
