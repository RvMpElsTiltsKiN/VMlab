import type {
  CreateVmRequest,
  CreatedVm,
  PaletteItem,
  VmDiskBus,
  VmFirmware,
  VmGraphics,
  VmOsDefaults,
  VmOsEntry,
  VmOsType,
} from '../types';

/** Valor del desplegable «Otro sistema…». */
export const OTHER_OS = 'other';
const CATALOG_PREFIX = 'cat-';

/**
 * Clave única de una entrada del catálogo. El id de VirtualBox NO sirve:
 * Ubuntu y Pop!_OS comparten `Ubuntu_64`, y Debian y Kali comparten `Debian_64`.
 */
export const catalogChoice = (index: number): string => `${CATALOG_PREFIX}${index}`;

/** Resuelve la entrada seleccionada por índice, nunca por id. */
export function entryFromChoice(catalog: VmOsEntry[], choice: string): VmOsEntry | null {
  if (!choice.startsWith(CATALOG_PREFIX)) return null;
  const index = Number(choice.slice(CATALOG_PREFIX.length));
  if (!Number.isInteger(index) || index < 0) return null;
  return catalog[index] ?? null;
}

/** ID de VirtualBox que se va a crear, derivado siempre de la selección visible. */
export function ostypeFromChoice(catalog: VmOsEntry[], choice: string, otherOstype: string): string {
  const entry = entryFromChoice(catalog, choice);
  if (entry) return entry.id;
  return choice === OTHER_OS ? otherOstype : '';
}

/** Valores genéricos para un `ostype` que no está en el catálogo. */
export function genericVmDefaults(family: VmOsType['family']): VmOsDefaults {
  return {
    memoryMb: 2048,
    cpus: 2,
    diskGb: 20,
    vramMb: 128,
    firmware: 'bios',
    tpm: false,
    graphics: family === 'windows' ? 'vboxsvga' : 'vmsvga',
    diskBus: 'sata',
  };
}

/** Defaults recomendados de la entrada visible del catálogo (o de «Otro sistema…»). */
export function defaultsFromChoice(
  catalog: VmOsEntry[],
  choice: string,
  family: VmOsType['family'] = 'other',
): VmOsDefaults {
  const entry = entryFromChoice(catalog, choice);
  if (entry) return { ...entry.defaults };
  return genericVmDefaults(choice === OTHER_OS ? family : 'other');
}

/** Los cinco ajustes delicados que el modo experto puede cambiar. */
export interface AdvancedVmSettings {
  vramMb: number;
  firmware: VmFirmware;
  tpm: boolean;
  graphics: VmGraphics;
  diskBus: VmDiskBus;
}

/** Proyección de los defaults del sistema sobre esos cinco ajustes. */
export function recommendedAdvanced(defaults: VmOsDefaults): AdvancedVmSettings {
  return {
    vramMb: defaults.vramMb,
    firmware: defaults.firmware,
    tpm: defaults.firmware === 'efi' && defaults.tpm,
    graphics: defaults.graphics,
    diskBus: defaults.diskBus,
  };
}

/**
 * Sin modo experto, los cinco ajustes viajan SIEMPRE con los valores
 * recomendados. Con modo experto se respetan los valores del formulario, pero
 * la TPM sigue exigiendo EFI como en el backend.
 */
export function effectiveAdvanced(
  defaults: VmOsDefaults,
  expertMode: boolean,
  custom: AdvancedVmSettings,
): AdvancedVmSettings {
  return expertMode
    ? { ...custom, tpm: custom.firmware === 'efi' && custom.tpm }
    : recommendedAdvanced(defaults);
}

/** Estado visible del formulario que se envía a `vm_create`. */
export interface VmRequestForm {
  name: string;
  choice: string;
  otherOstype: string;
  memoryMb: number;
  cpus: number;
  diskGb: number;
  vramMb: number;
  firmware: VmFirmware;
  tpm: boolean;
  graphics: VmGraphics;
  diskBus: VmDiskBus;
  isoPath: string | null;
  /** Si es falso, los cinco ajustes delicados se toman de `recommended`. */
  expertMode?: boolean;
  /** Defaults del sistema actualmente seleccionado. */
  recommended?: VmOsDefaults;
  /** Familia del `ostype` elegido en «Otro sistema…». */
  osFamily?: VmOsType['family'];
}

/** Construye la petición: `ostype` y ajustes salen de la entrada realmente elegida. */
export function buildCreateVmRequest(form: VmRequestForm, catalog: VmOsEntry[]): CreateVmRequest {
  const ostype = ostypeFromChoice(catalog, form.choice, form.otherOstype);
  if (!ostype) throw new Error('No se ha seleccionado un tipo de sistema operativo');
  const defaults = form.recommended ?? defaultsFromChoice(catalog, form.choice, form.osFamily);
  const advanced = effectiveAdvanced(defaults, !!form.expertMode, {
    vramMb: form.vramMb,
    firmware: form.firmware,
    tpm: form.tpm,
    graphics: form.graphics,
    diskBus: form.diskBus,
  });
  return {
    name: form.name,
    ostype,
    memoryMb: form.memoryMb,
    cpus: form.cpus,
    diskGb: form.diskGb,
    vramMb: advanced.vramMb,
    firmware: advanced.firmware,
    tpm: advanced.tpm,
    graphics: advanced.graphics,
    diskBus: advanced.diskBus,
    ...(form.isoPath ? { iso: form.isoPath } : {}),
  };
}

/**
 * Nodo del lienzo para una VM recién creada. La etiqueta es el nombre real de
 * la VM (`created.name`), nunca la etiqueta del sistema operativo elegido.
 */
export function machineFromCreatedVm(created: Pick<CreatedVm, 'uuid' | 'name'>): PaletteItem {
  return { kind: 'vbox', label: created.name, target: created.uuid };
}
