import type { LinkConditions } from '../types';

/** Límites compartidos con `validate::link_conditions` del núcleo Rust. */
export const LINK_LIMITS = {
  delayMs: 10_000,
  jitterMs: 10_000,
  lossTenths: 1_000,
  rateKbitMin: 8,
  rateKbitMax: 1_000_000,
} as const;

/** `true` si las condiciones hacen algo (latencia, pérdida, variación o límite). */
export function hasLinkConditions(link: LinkConditions | undefined): link is LinkConditions {
  if (!link) return false;
  return (
    link.delayMs > 0 ||
    link.jitterMs > 0 ||
    link.lossTenths > 0 ||
    (link.rateKbit !== undefined && link.rateKbit > 0)
  );
}

/** Décimas de porcentaje a texto: 15 -> `1,5`, 20 -> `2`. */
export function lossPercent(tenths: number): string {
  if (Number.isInteger(tenths / 10)) return String(tenths / 10);
  return (tenths / 10).toFixed(1);
}

/** kbit/s a una etiqueta compacta: 20000 -> `20 Mbit/s`. */
export function rateLabel(rateKbit: number | undefined): string | null {
  if (!rateKbit) return null;
  return rateKbit % 1000 === 0 ? `${rateKbit / 1000} Mbit/s` : `${rateKbit} kbit/s`;
}

/** Resumen corto para la insignia del nodo: `50 ms · 1 %` o `20 Mbit/s`. */
export function linkSummary(link: LinkConditions | undefined): string {
  if (!hasLinkConditions(link)) return '';
  const parts: string[] = [];
  if (link.delayMs > 0) {
    parts.push(link.jitterMs > 0 ? `${link.delayMs} ± ${link.jitterMs} ms` : `${link.delayMs} ms`);
  }
  if (link.lossTenths > 0) parts.push(`${lossPercent(link.lossTenths)} %`);
  const rate = rateLabel(link.rateKbit);
  if (rate) parts.push(rate);
  return parts.join(' · ');
}

/** Compara dos juegos de condiciones (sin valor = todo a cero y sin límite). */
export function sameLink(a: LinkConditions | undefined, b: LinkConditions | undefined): boolean {
  return (
    (a?.delayMs ?? 0) === (b?.delayMs ?? 0) &&
    (a?.jitterMs ?? 0) === (b?.jitterMs ?? 0) &&
    (a?.lossTenths ?? 0) === (b?.lossTenths ?? 0) &&
    (a?.rateKbit ?? undefined) === (b?.rateKbit ?? undefined)
  );
}
