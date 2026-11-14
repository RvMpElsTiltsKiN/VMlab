import type { Update } from '@tauri-apps/plugin-updater';
import { isTauri } from './ipc';

/** Ajuste persistente: comprobar actualizaciones al iniciar (activado por defecto). */
export const CHECK_UPDATES_KEY = 'vmlab.checkUpdates';

export function checkUpdatesOnStartup(): boolean {
  try {
    return localStorage.getItem(CHECK_UPDATES_KEY) !== '0';
  } catch {
    return true;
  }
}

export function setCheckUpdatesOnStartup(enabled: boolean): void {
  try {
    localStorage.setItem(CHECK_UPDATES_KEY, enabled ? '1' : '0');
  } catch {
    /* sin almacenamiento */
  }
}

/**
 * Busca una actualización. Devuelve `null` en modo navegador, sin red o ante
 * cualquier error del updater: comprobar nunca debe interrumpir la app.
 */
export async function checkForUpdate(): Promise<Update | null> {
  if (!isTauri) return null;
  try {
    const { check } = await import('@tauri-apps/plugin-updater');
    return await check();
  } catch {
    return null;
  }
}
