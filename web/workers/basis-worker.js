// Basis codec worker — hosts the vendored Basis Universal modules
// (web/vendor/basis/) in an isolated scope, off the main thread.
//
// Both vendored builds are Emscripten MODULARIZE+UMD and export the SAME
// global factory name `BASIS`, so each importScripts() is followed by a
// capture-and-delete of the global (see web/vendor/basis/README.md).
//
// Protocol (versioned; every request carries a client-chosen `id` echoed in
// the reply):
//   → { v, id, op: "init", urls: { transcoder, encoder? } }
//   → { v, id, op: "ping" }
//   → { v, id, op: "transcode", ktx2: ArrayBuffer, target: <name>, layer?, face? }
//   → { v, id, op: "encode", rgba: ArrayBuffer, width, height,
//       uastc, srgb, mipmaps, quality?, zstd? }            (editor-only path)
//   ← { v, id, ok: true, result: {...} }   (level/ktx2 buffers transferred)
//   ← { v, id, ok: false, error: { code, message } }
//
// The worker never restarts itself — the client owns restart-on-fatal.
'use strict';

const PROTOCOL_VERSION = 1;

// Hard input limits — untrusted containers must not be able to make the
// worker allocate unbounded memory. Structured errors, never crashes.
const MAX_KTX2_BYTES = 64 * 1024 * 1024;
const MAX_ENCODE_PIXELS = 4096 * 4096;
const MAX_TEXTURE_DIMENSION = 16384;

let urls = null; // set by "init"
const modulePromises = { transcoder: null, encoder: null };

class WorkerError extends Error {
    constructor(code, message) {
        super(message);
        this.code = code;
    }
}

function instantiateModule(jsUrl) {
    // importScripts is synchronous; capture the shared global, then clear it.
    // (It's a `var` declaration — non-configurable, so assign undefined
    // rather than `delete`.)
    importScripts(jsUrl);
    const factory = self.BASIS;
    self.BASIS = undefined;
    if (typeof factory !== 'function') {
        throw new WorkerError('module-load', `no BASIS factory exported by ${jsUrl}`);
    }
    const wasmUrl = jsUrl.replace(/\.js(\?.*)?$/, '.wasm$1');
    return factory({
        locateFile: (file) => (file.endsWith('.wasm') ? wasmUrl : file),
    }).then((module) => {
        if (typeof module.initializeBasis === 'function') {
            module.initializeBasis();
        }
        return module;
    });
}

function getModule(kind) {
    if (!urls) {
        throw new WorkerError('not-initialized', 'send an "init" request first');
    }
    if (!modulePromises[kind]) {
        const jsUrl = urls[kind];
        if (!jsUrl) {
            throw new WorkerError('module-unavailable', `no ${kind} URL was configured at init`);
        }
        modulePromises[kind] = instantiateModule(jsUrl);
    }
    return modulePromises[kind];
}

// Transcoder format names → the module's embind enum. Resolved at runtime so
// we never depend on raw enum integers staying stable across Basis versions.
const TARGET_TO_ENUM = {
    'astc-4x4': 'cTFASTC_4x4_RGBA',
    'bc7': 'cTFBC7_RGBA',
    'etc2-rgba': 'cTFETC2_RGBA',
    'etc1-rgb': 'cTFETC1_RGB',
    'bc3': 'cTFBC3_RGBA',
    'bc1': 'cTFBC1_RGB',
    'bc5': 'cTFBC5_RG',
    'eac-rg11': 'cTFETC2_EAC_RG11',
    'rgba32': 'cTFRGBA32',
};

function resolveTargetFormat(module, target) {
    const enumName = TARGET_TO_ENUM[target];
    if (!enumName) {
        throw new WorkerError('bad-target', `unknown transcode target "${target}"`);
    }
    const entry = module.transcoder_texture_format?.[enumName];
    if (entry === undefined) {
        throw new WorkerError(
            'bad-target',
            `this transcoder build has no ${enumName} (target "${target}")`
        );
    }
    // embind enum values carry .value; tolerate raw ints just in case.
    return typeof entry === 'object' ? entry.value : entry;
}

async function handleTranscode(req) {
    const module = await getModule('transcoder');
    if (!(req.ktx2 instanceof ArrayBuffer) || req.ktx2.byteLength === 0) {
        throw new WorkerError('bad-request', 'transcode needs a non-empty ktx2 ArrayBuffer');
    }
    if (req.ktx2.byteLength > MAX_KTX2_BYTES) {
        throw new WorkerError(
            'too-large',
            `ktx2 input ${req.ktx2.byteLength} bytes exceeds the ${MAX_KTX2_BYTES} limit`
        );
    }
    const format = resolveTargetFormat(module, req.target);
    const layer = req.layer ?? 0;
    const face = req.face ?? 0;

    const ktx2File = new module.KTX2File(new Uint8Array(req.ktx2));
    try {
        if (!ktx2File.isValid()) {
            throw new WorkerError('bad-ktx2', 'not a valid Basis KTX2 file');
        }
        if (
            ktx2File.getWidth() > MAX_TEXTURE_DIMENSION ||
            ktx2File.getHeight() > MAX_TEXTURE_DIMENSION
        ) {
            throw new WorkerError(
                'too-large',
                `${ktx2File.getWidth()}x${ktx2File.getHeight()} exceeds the ${MAX_TEXTURE_DIMENSION} dimension limit`
            );
        }
        const levels = ktx2File.getLevels();
        const layers = ktx2File.getLayers();
        const faces = ktx2File.getFaces();
        if (layer >= Math.max(layers, 1) || face >= faces) {
            throw new WorkerError(
                'unsupported-layout',
                `requested layer ${layer}/face ${face} out of range (${layers} layers, ${faces} faces)`
            );
        }
        if (!ktx2File.startTranscoding()) {
            throw new WorkerError('transcode-failed', 'startTranscoding failed');
        }

        const outLevels = [];
        const transfer = [];
        for (let level = 0; level < levels; level++) {
            const info = ktx2File.getImageLevelInfo(level, layer, face);
            const size = ktx2File.getImageTranscodedSizeInBytes(level, layer, face, format);
            const dst = new Uint8Array(size);
            if (!ktx2File.transcodeImage(dst, level, layer, face, format, 0, -1, -1)) {
                throw new WorkerError('transcode-failed', `transcodeImage failed at level ${level}`);
            }
            outLevels.push({
                level,
                width: info.origWidth ?? info.width,
                height: info.origHeight ?? info.height,
                data: dst.buffer,
            });
            transfer.push(dst.buffer);
        }

        return {
            result: {
                target: req.target,
                width: ktx2File.getWidth(),
                height: ktx2File.getHeight(),
                hasAlpha: ktx2File.getHasAlpha(),
                isUastc: ktx2File.isUASTC(),
                levels: outLevels,
            },
            transfer,
        };
    } finally {
        ktx2File.close();
        ktx2File.delete();
    }
}

async function handleEncode(req) {
    const module = await getModule('encoder');
    const { rgba, width, height } = req;
    if (!(rgba instanceof ArrayBuffer) || rgba.byteLength !== width * height * 4) {
        throw new WorkerError(
            'bad-request',
            `encode needs an rgba ArrayBuffer of exactly width*height*4 bytes (got ${rgba?.byteLength}, want ${width * height * 4})`
        );
    }

    if (width * height > MAX_ENCODE_PIXELS || width > MAX_TEXTURE_DIMENSION || height > MAX_TEXTURE_DIMENSION) {
        throw new WorkerError(
            'too-large',
            `${width}x${height} exceeds the encode limit (${MAX_ENCODE_PIXELS} pixels / ${MAX_TEXTURE_DIMENSION} per side)`
        );
    }

    const encoder = new module.BasisEncoder();
    try {
        encoder.setCreateKTX2File(true);
        encoder.setDebug(false);
        encoder.setComputeStats(false);
        encoder.setSliceSourceImage(0, new Uint8Array(rgba), width, height, false);
        encoder.setUASTC(!!req.uastc);
        if (req.uastc) {
            encoder.setKTX2UASTCSupercompression(req.zstd !== false);
        } else {
            // ETC1S quality 1..255 (Basis default 128).
            encoder.setQualityLevel(req.quality ?? 128);
        }
        // Color textures are sRGB/perceptual; data textures (normals etc.) linear.
        // Some setters are version-dependent (v2 renamed/dropped a few) —
        // apply what this build exposes.
        const trySet = (name, ...args) => {
            if (typeof encoder[name] === 'function') encoder[name](...args);
        };
        trySet('setPerceptual', !!req.srgb);
        trySet('setMipSRGB', !!req.srgb);
        trySet('setKTX2SRGBTransferFunc', !!req.srgb);
        trySet('setSRGB', !!req.srgb);
        encoder.setMipGen(!!req.mipmaps);
        // Detect alpha rather than force it: an opaque source (A all 255) ships
        // NO alpha slice, so the container reports no alpha and the loader can
        // pick the 0.5 B/px opaque rung (BC1 / ETC2-RGB — the 8× VRAM win).
        // This is basisu's default; set it explicitly so the contract is local.
        trySet('setCheckForAlpha', true);
        trySet('setForceAlpha', false);

        // Worst-case output: raw size + generous container overhead.
        const dst = new Uint8Array(width * height * 4 + (1 << 20));
        const written = encoder.encode(dst);
        if (!written) {
            throw new WorkerError('encode-failed', 'BasisEncoder.encode returned 0 bytes');
        }
        const ktx2 = dst.slice(0, written).buffer;
        return { result: { ktx2, width, height }, transfer: [ktx2] };
    } finally {
        encoder.delete();
    }
}

self.onmessage = async (event) => {
    const req = event.data || {};
    const reply = { v: PROTOCOL_VERSION, id: req.id };
    try {
        if (req.v !== PROTOCOL_VERSION) {
            throw new WorkerError(
                'bad-protocol',
                `protocol v${req.v} not supported (worker is v${PROTOCOL_VERSION})`
            );
        }
        let outcome;
        switch (req.op) {
            case 'init':
                if (!req.urls || !req.urls.transcoder) {
                    throw new WorkerError('bad-request', 'init needs urls.transcoder');
                }
                urls = req.urls;
                // Warm the transcoder eagerly; the encoder stays lazy.
                await getModule('transcoder');
                outcome = { result: { ready: true }, transfer: [] };
                break;
            case 'ping':
                outcome = { result: { pong: true }, transfer: [] };
                break;
            case 'transcode':
                outcome = await handleTranscode(req);
                break;
            case 'encode':
                outcome = await handleEncode(req);
                break;
            default:
                throw new WorkerError('bad-request', `unknown op "${req.op}"`);
        }
        reply.ok = true;
        reply.result = outcome.result;
        self.postMessage(reply, outcome.transfer);
    } catch (e) {
        reply.ok = false;
        reply.error = {
            code: e instanceof WorkerError ? e.code : 'internal',
            message: String((e && e.message) || e),
        };
        self.postMessage(reply);
    }
};
