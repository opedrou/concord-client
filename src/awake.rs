//! Monitor e sistema acesos enquanto se assiste uma transmissão. O Windows não vê a
//! transmissão como mídia, e sem mouse ou teclado a tela apagava no meio.
//! No Linux não faz nada ainda (portal de Inhibit, se um dia pedirem).
use std::sync::atomic::{AtomicBool, Ordering};

static ON: AtomicBool = AtomicBool::new(false);

/// Liga ou desliga. Só fala com o sistema quando muda. `SetThreadExecutionState` vale por
/// thread: sempre a da interface, que é quem chama.
pub fn keep(on: bool) {
    #[cfg(windows)]
    if ON.swap(on, Ordering::Relaxed) != on {
        use windows::Win32::System::Power::{
            ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
        };
        let flags = if on {
            ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        // SAFETY: só troca flags de energia da thread atual.
        unsafe { SetThreadExecutionState(flags) };
    }
    #[cfg(not(windows))]
    ON.store(on, Ordering::Relaxed);
}
