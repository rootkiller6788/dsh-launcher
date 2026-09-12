/**
 * Normalize a Tauri `invoke` rejection into code + message + next action.
 *
 * Commands return `AppError`, which serializes as a `CodedError` object
 * `{ code, category, title, message, nextAction }`. Non-AppError rejections
 * (a JS-thrown error, a `tauri::Error` string) fall through to the string path.
 */

export interface CodedError {
  code?: string
  category?: string
  title?: string
  message?: string
  nextAction?: string
}

export interface DescribedError {
  code: string | null
  message: string
  nextAction: string | null
}

export function describeError(e: unknown): DescribedError {
  if (e && typeof e === 'object' && 'message' in e) {
    const c = e as CodedError
    return {
      code: typeof c.code === 'string' ? c.code : null,
      message: typeof c.message === 'string' ? c.message : String(e),
      nextAction: typeof c.nextAction === 'string' ? c.nextAction : null,
    }
  }
  return { code: null, message: String(e), nextAction: null }
}
