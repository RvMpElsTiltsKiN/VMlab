// Verificación sencilla de la lógica pura de selección de `NewVmDialog`.
// No añade dependencias: reutiliza el `typescript` ya instalado para transpilar
// `src/lib/vmRequest.ts` en memoria y ejecuta aserciones con `node:assert`.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const url = new URL('../src/lib/vmRequest.ts', import.meta.url);
const source = readFileSync(url, 'utf8');
const { outputText, diagnostics = [] } = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 },
  fileName: fileURLToPath(url),
});
assert.equal(diagnostics.length, 0, 'la transpilación de vmRequest.ts no debe dar errores');

const api = {};
new Function('exports', outputText)(api);
const {
  OTHER_OS,
  catalogChoice,
  entryFromChoice,
  ostypeFromChoice,
  buildCreateVmRequest,
  machineFromCreatedVm,
  defaultsFromChoice,
  recommendedAdvanced,
  effectiveAdvanced,
} = api;

const baseDefaults = {
  memoryMb: 2048, cpus: 2, diskGb: 20, vramMb: 128,
  firmware: 'bios', tpm: false, graphics: 'vmsvga', diskBus: 'sata',
};
const catalog = [
  {
    id: 'Windows11_64', label: 'Windows 11 (64 bits)', group: 'windows',
    defaults: { ...baseDefaults, firmware: 'efi', tpm: true, graphics: 'vboxsvga' },
  },
  { id: 'Ubuntu_64', label: 'Ubuntu', group: 'linux', defaults: { ...baseDefaults } },
  { id: 'Ubuntu_64', label: 'Pop!_OS', group: 'linux', defaults: { ...baseDefaults } },
  { id: 'Debian_64', label: 'Debian', group: 'linux', defaults: { ...baseDefaults } },
  { id: 'Debian_64', label: 'Kali Linux', group: 'linux', defaults: { ...baseDefaults } },
];
const form = {
  name: 'Nueva VM',
  choice: '',
  otherOstype: '',
  memoryMb: 2048,
  cpus: 2,
  diskGb: 20,
  vramMb: 128,
  firmware: 'bios',
  tpm: false,
  graphics: 'vmsvga',
  diskBus: 'sata',
  isoPath: null,
};

// Ids repetidos: la clave única es el índice, no el id de VirtualBox.
assert.equal(entryFromChoice(catalog, catalogChoice(1)).label, 'Ubuntu');
assert.equal(entryFromChoice(catalog, catalogChoice(2)).label, 'Pop!_OS');
assert.equal(entryFromChoice(catalog, catalogChoice(4)).label, 'Kali Linux');
assert.equal(ostypeFromChoice(catalog, catalogChoice(2), ''), 'Ubuntu_64');

// El ostype enviado es el de la entrada elegida, nunca el de la primera.
assert.equal(buildCreateVmRequest({ ...form, choice: catalogChoice(1) }, catalog).ostype, 'Ubuntu_64');
assert.equal(buildCreateVmRequest({ ...form, choice: catalogChoice(4) }, catalog).ostype, 'Debian_64');

// Los ajustes visibles viajan tal cual (y la TPM exige EFI).
const win11 = buildCreateVmRequest({
  ...form,
  choice: catalogChoice(0),
  firmware: 'efi',
  tpm: true,
  graphics: 'vboxsvga',
  diskBus: 'sata',
  isoPath: 'C:/iso/win11.iso',
}, catalog);
assert.equal(win11.ostype, 'Windows11_64');
assert.equal(win11.firmware, 'efi');
assert.equal(win11.tpm, true);
assert.equal(win11.graphics, 'vboxsvga');
assert.equal(win11.diskBus, 'sata');
assert.equal(win11.iso, 'C:/iso/win11.iso');
assert.equal(buildCreateVmRequest({ ...form, choice: catalogChoice(1), firmware: 'bios', tpm: true }, catalog).tpm, false);

// Cambiar de sistema sin modo experto cambia firmware/TPM a los del nuevo sistema.
assert.equal(defaultsFromChoice(catalog, catalogChoice(0)).firmware, 'efi');
assert.equal(defaultsFromChoice(catalog, catalogChoice(0)).tpm, true);
assert.equal(defaultsFromChoice(catalog, catalogChoice(1)).firmware, 'bios');
assert.equal(defaultsFromChoice(catalog, catalogChoice(1)).tpm, false);
assert.deepEqual(recommendedAdvanced(defaultsFromChoice(catalog, catalogChoice(1))), {
  vramMb: 128, firmware: 'bios', tpm: false, graphics: 'vmsvga', diskBus: 'sata',
});

// Sin modo experto, los cinco ajustes delicados viajan con los defaults aunque
// el estado del formulario conserve valores anteriores (p. ej. Win11 → Ubuntu).
const switched = buildCreateVmRequest({
  ...form,
  choice: catalogChoice(1),
  firmware: 'efi',
  tpm: true,
  graphics: 'vboxsvga',
  vramMb: 256,
  recommended: defaultsFromChoice(catalog, catalogChoice(1)),
}, catalog);
assert.equal(switched.firmware, 'bios');
assert.equal(switched.tpm, false);
assert.equal(switched.graphics, 'vmsvga');
assert.equal(switched.vramMb, 128);

// Con modo experto se respetan los valores editados, pero TPM exige EFI.
const custom = {
  vramMb: 64, firmware: 'efi', tpm: true, graphics: 'vboxvga', diskBus: 'ide',
};
assert.deepEqual(effectiveAdvanced(defaultsFromChoice(catalog, catalogChoice(1)), true, custom), custom);
assert.equal(effectiveAdvanced(defaultsFromChoice(catalog, catalogChoice(1)), true, { ...custom, firmware: 'bios' }).tpm, false);
const expertRequest = buildCreateVmRequest({
  ...form,
  ...custom,
  choice: catalogChoice(1),
  expertMode: true,
  recommended: defaultsFromChoice(catalog, catalogChoice(1)),
}, catalog);
assert.equal(expertRequest.firmware, 'efi');
assert.equal(expertRequest.tpm, true);
assert.equal(expertRequest.graphics, 'vboxvga');
assert.equal(expertRequest.diskBus, 'ide');

// Una selección anterior de «Otro sistema…» no contamina la del catálogo.
assert.equal(
  buildCreateVmRequest({ ...form, choice: catalogChoice(1), otherOstype: 'Windows11_64' }, catalog).ostype,
  'Ubuntu_64',
);
assert.equal(buildCreateVmRequest({ ...form, choice: OTHER_OS, otherOstype: 'FreeBSD_64' }, catalog).ostype, 'FreeBSD_64');

// Sin selección válida no se construye la petición.
assert.throws(() => buildCreateVmRequest({ ...form, choice: catalogChoice(99) }, catalog), /tipo de sistema/);

// El nodo del lienzo se etiqueta con el nombre real de la VM.
assert.deepEqual(machineFromCreatedVm({ uuid: 'uuid-1', name: 'Nueva VM', cfgFile: '', vdiPath: '' }), {
  kind: 'vbox',
  label: 'Nueva VM',
  target: 'uuid-1',
});

console.log('check-vm-request: OK');
