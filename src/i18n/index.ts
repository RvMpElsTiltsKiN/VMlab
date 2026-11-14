import { useSyncExternalStore } from 'react';
import { en } from './en';
import { es } from './es';

/** Idioma elegido por el usuario. `auto` sigue el idioma del sistema. */
export type Lang = 'auto' | 'es' | 'en';

/** Idioma efectivo de la interfaz. */
export type Locale = 'es' | 'en';

export type TKey = keyof typeof es;

/** Parámetros de interpolación (`{nombre}`). `count` activa el plural simple. */
export type TParams = Record<string, string | number>;

export type Translate = (key: TKey, params?: TParams) => string;

/**
 * Mensaje sin traducir. Los módulos que no son componentes (validadores,
 * simulador, store) devuelven claves + parámetros para que el texto se componga
 * con el idioma activo en el momento de mostrarlo.
 */
export interface Message {
  key: TKey;
  params?: TParams;
}

const LANG_KEY = 'vmlab.lang';

const DICTS: Record<Locale, Record<TKey, string>> = { es, en };

/** `es*` del sistema → español; cualquier otro idioma → inglés. */
export function resolveLang(lang: Lang): Locale {
  if (lang === 'es' || lang === 'en') return lang;
  const nav = typeof navigator !== 'undefined' ? navigator.language : '';
  return /^es/i.test(nav) ? 'es' : 'en';
}

function storedLang(): Lang {
  try {
    const saved = localStorage.getItem(LANG_KEY);
    if (saved === 'auto' || saved === 'es' || saved === 'en') return saved;
  } catch {
    /* sin almacenamiento */
  }
  return 'auto';
}

let activeLocale: Locale = resolveLang(storedLang());
const listeners = new Set<() => void>();

/**
 * Aplica un idioma desde el store: lo guarda, actualiza `<html lang>` y avisa a
 * los componentes suscritos. i18n no importa el store para evitar ciclos.
 */
export function setI18nLang(lang: Lang): void {
  try { localStorage.setItem(LANG_KEY, lang); } catch { /* sin almacenamiento */ }
  activeLocale = resolveLang(lang);
  if (typeof document !== 'undefined') document.documentElement.lang = activeLocale;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

/** Idioma efectivo ahora mismo, sin suscribirse a cambios. */
export function currentLocale(): Locale {
  return activeLocale;
}

function interpolate(text: string, params?: TParams): string {
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (match, name: string) =>
    Object.prototype.hasOwnProperty.call(params, name) ? String(params[name]) : match,
  );
}

function translate(locale: Locale, key: TKey, params?: TParams): string {
  const raw = DICTS[locale][key];
  const count = params?.count;
  // Plural simple: «singular|plural» cuando se pasa `count`.
  const text = typeof count === 'number' && raw.includes('|')
    ? raw.split('|')[count === 1 ? 0 : 1]
    : raw;
  return interpolate(text, params);
}

/** Traduce con el idioma activo. Pensado para helpers fuera de React. */
export function t(key: TKey, params?: TParams): string {
  return translate(activeLocale, key, params);
}

/** Compone un mensaje estructurado con el traductor indicado (por defecto, el activo). */
export function formatMessage(message: Message, translateFn: Translate = t): string {
  return translateFn(message.key, message.params);
}

/**
 * Marca un mensaje técnico del motor (Rust), que sigue en español. En inglés
 * añade una nota discreta para que el usuario sepa por qué no está traducido.
 */
export function technicalText(text: string): string {
  if (activeLocale !== 'en') return text;
  return `${text}\n${translate('en', 'toast.technicalNote')}`;
}

/** Devuelve `t` ligado al idioma actual y se vuelve a renderizar al cambiarlo. */
export function useT(): Translate {
  useSyncExternalStore(subscribe, () => activeLocale);
  return (key, params) => translate(activeLocale, key, params);
}

/** Devuelve el idioma efectivo y se vuelve a renderizar al cambiarlo. */
export function useLocale(): Locale {
  useSyncExternalStore(subscribe, () => activeLocale);
  return activeLocale;
}
