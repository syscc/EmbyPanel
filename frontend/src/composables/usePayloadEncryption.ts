import type * as Forge from 'node-forge'
import type { EncryptionPublicKey, PublicKeyResponse } from '@/types/panel'

type Api = <T>(path: string, init?: RequestInit) => Promise<T>

export function usePayloadEncryption(options: {
  api: Api
  translate: (source: string) => string
}) {
  let forgeRequest: Promise<typeof import('node-forge')> | undefined

  async function fetchPublicKey(): Promise<EncryptionPublicKey> {
    const response = await options.api<PublicKeyResponse>('/api/public-key')
    if (!hasWebCrypto()) {
      const forge = await loadForge()
      return {
        kind: 'forge',
        key: forge.pki.publicKeyFromPem(response.public_key_pem),
      }
    }
    return {
      kind: 'webcrypto',
      key: await crypto.subtle.importKey(
        'spki',
        pemToArrayBuffer(response.public_key_pem),
        { name: 'RSA-OAEP', hash: 'SHA-256' },
        false,
        ['encrypt'],
      ),
    }
  }

  async function encryptPayload(name: string, value: unknown) {
    const publicKey = await fetchPublicKey()
    const aesKey = randomBytes(32)
    const fieldName = randomFieldName()
    const iv = randomBytes(12)
    const plaintext = new TextEncoder().encode(JSON.stringify({ name, value }))
    const { encryptedKey, encrypted } = publicKey.kind === 'webcrypto'
      ? await encryptWithWebCrypto(publicKey.key, aesKey, iv, plaintext)
      : await encryptWithForge(publicKey.key, aesKey, iv, plaintext)
    return {
      encrypted_key: bytesToBase64Url(encryptedKey),
      fields: {
        [fieldName]: {
          iv: bytesToBase64Url(iv),
          value: bytesToBase64Url(encrypted),
        },
      },
    }
  }

  async function warmPublicKey() {
    await fetchPublicKey()
  }

  function randomId(prefix: string) {
    return `${prefix}-${bytesToBase64Url(randomBytes(8))}`
  }

  async function encryptWithWebCrypto(
    key: CryptoKey,
    aesKey: Uint8Array,
    iv: Uint8Array,
    plaintext: Uint8Array,
  ) {
    const cryptoKey = await crypto.subtle.importKey('raw', toArrayBuffer(aesKey), 'AES-GCM', false, [
      'encrypt',
    ])
    const encryptedKey = await crypto.subtle.encrypt(
      { name: 'RSA-OAEP' },
      key,
      toArrayBuffer(aesKey),
    )
    const encrypted = await crypto.subtle.encrypt(
      { name: 'AES-GCM', iv: toArrayBuffer(iv) },
      cryptoKey,
      toArrayBuffer(plaintext),
    )
    return {
      encryptedKey: new Uint8Array(encryptedKey),
      encrypted: new Uint8Array(encrypted),
    }
  }

  async function encryptWithForge(
    key: Forge.pki.rsa.PublicKey,
    aesKey: Uint8Array,
    iv: Uint8Array,
    plaintext: Uint8Array,
  ) {
    const forge = await loadForge()
    const encryptedKey = key.encrypt(bytesToBinary(aesKey), 'RSA-OAEP', {
      md: forge.md.sha256.create(),
      mgf1: {
        md: forge.md.sha256.create(),
      },
    })
    const cipher = forge.cipher.createCipher('AES-GCM', bytesToBinary(aesKey))
    cipher.start({ iv: bytesToBinary(iv), tagLength: 128 })
    cipher.update(forge.util.createBuffer(bytesToBinary(plaintext)))
    if (!cipher.finish()) throw new Error(options.translate('加密请求失败'))
    return {
      encryptedKey: binaryToBytes(encryptedKey),
      encrypted: binaryToBytes(cipher.output.getBytes() + cipher.mode.tag.getBytes()),
    }
  }

  function randomFieldName() {
    return bytesToBase64Url(randomBytes(12))
  }

  function hasWebCrypto() {
    return Boolean(
      typeof window !== 'undefined'
        && window.isSecureContext
        && globalThis.crypto?.subtle
        && globalThis.crypto?.getRandomValues,
    )
  }

  function randomBytes(length: number) {
    const bytes = new Uint8Array(length)
    if (globalThis.crypto?.getRandomValues) return globalThis.crypto.getRandomValues(bytes)
    throw new Error(options.translate('加密请求失败'))
  }

  function loadForge() {
    forgeRequest ??= import('node-forge')
    return forgeRequest
  }

  function pemToArrayBuffer(pem: string) {
    const base64 = pem
      .replace('-----BEGIN PUBLIC KEY-----', '')
      .replace('-----END PUBLIC KEY-----', '')
      .replace(/\s/g, '')
    const binary = atob(base64)
    const bytes = new Uint8Array(binary.length)
    for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index)
    return bytes.buffer
  }

  function bytesToBase64Url(bytes: Uint8Array) {
    let binary = ''
    bytes.forEach((byte) => {
      binary += String.fromCharCode(byte)
    })
    return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '')
  }

  function bytesToBinary(bytes: Uint8Array) {
    let binary = ''
    bytes.forEach((byte) => {
      binary += String.fromCharCode(byte)
    })
    return binary
  }

  function binaryToBytes(binary: string) {
    const bytes = new Uint8Array(binary.length)
    for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index)
    return bytes
  }

  function toArrayBuffer(bytes: Uint8Array) {
    const buffer = new ArrayBuffer(bytes.byteLength)
    new Uint8Array(buffer).set(bytes)
    return buffer
  }

  return { encryptPayload, warmPublicKey, randomId }
}
