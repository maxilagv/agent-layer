/**
 * Sign-in view for the loopback web surface.
 *
 * The operator enters the single-use code that the server printed in the
 * terminal. The code is exchanged for an HttpOnly session cookie; the
 * browser never stores a bearer token.
 *
 * The background is an original, self-contained WebGL space scene; the
 * watcher above the card is an original vector mark inspired by the mascot.
 * Neither the renderer nor the mark loads an external asset, so the page keeps
 * the loopback surface's `default-src 'self'` policy.
 */
import { useEffect, useRef, useState, type FormEvent } from 'react';
import { errorMessage } from '../api/client';
import { CosmicScene } from './CosmicScene';
import { Watcher } from './Watcher';

export interface LoginViewProps {
  onSubmit: (code: string) => Promise<void>;
  error?: string | null;
}

export function LoginView({ onSubmit, error }: LoginViewProps) {
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);
  const rootRef = useRef<HTMLElement>(null);

  // Scroll and pointer parallax. We write transforms directly to the few
  // layers that move instead of updating inherited CSS variables on the
  // root: that avoids a style recalculation across the whole scene per frame.
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;

    const reduceMotion =
      typeof window.matchMedia === 'function' &&
      window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    if (reduceMotion) return;

    const watcher = root.querySelector<HTMLElement>('.login__watcher');
    const card = root.querySelector<HTMLElement>('.login__card');
    const sheen = root.querySelector<HTMLElement>('.login__card-sheen');
    const cue = root.querySelector<HTMLElement>('.login__scroll-cue');
    const pupils = Array.from(root.querySelectorAll<HTMLElement>('.login__pupil'));

    if (!watcher || !card) return;

    const round = (value: number) => Math.round(value * 1000) / 1000;
    let frame = 0;
    let pointerX = 0;
    let pointerY = 0;

    const apply = () => {
      frame = 0;
      const travel = Math.max(1, window.innerHeight * 0.72);
      const scroll = Math.min(1, Math.max(0, window.scrollY / travel));
      const px = pointerX;
      const py = pointerY;

      watcher.style.transform = `translate3d(${round(px * -10)}px, ${round(
        py * -6 + scroll * 46,
      )}px, 0) rotate(${round(px * -1.2 + scroll * 1.4)}deg) scale(${round(
        1 + scroll * 0.035,
      )})`;

      card.style.transform = `translateY(${round(scroll * -6)}px) perspective(1200px) rotateX(${round(
        py * -2.2,
      )}deg) rotateY(${round(px * 2.8)}deg)`;

      for (const pupil of pupils) {
        pupil.style.transform = `translate(${round(px * 3.4)}px, ${round(
          py * 2.2 + scroll * 4.2,
        )}px)`;
      }

      if (sheen) {
        sheen.style.transform = `translate3d(${round(px * 46)}px, ${round(py * 34)}px, 0)`;
      }
      if (cue) {
        cue.style.opacity = String(round(Math.max(0, 1 - scroll * 1.7)));
      }
    };

    const schedule = () => {
      if (frame === 0) frame = window.requestAnimationFrame(apply);
    };

    const onPointerMove = (event: PointerEvent) => {
      pointerX = (event.clientX / Math.max(1, window.innerWidth) - 0.5) * 2;
      pointerY = (event.clientY / Math.max(1, window.innerHeight) - 0.5) * 2;
      schedule();
    };

    const onPointerLeave = () => {
      pointerX = 0;
      pointerY = 0;
      schedule();
    };

    apply();
    window.addEventListener('pointermove', onPointerMove, { passive: true });
    window.addEventListener('scroll', schedule, { passive: true });
    window.addEventListener('resize', schedule, { passive: true });
    window.addEventListener('blur', onPointerLeave);
    document.documentElement.addEventListener('pointerleave', onPointerLeave);

    return () => {
      if (frame !== 0) window.cancelAnimationFrame(frame);
      window.removeEventListener('pointermove', onPointerMove);
      window.removeEventListener('scroll', schedule);
      window.removeEventListener('resize', schedule);
      window.removeEventListener('blur', onPointerLeave);
      document.documentElement.removeEventListener('pointerleave', onPointerLeave);
    };
  }, []);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const trimmed = code.trim();
    if (trimmed.length === 0) {
      setLocalError('Enter the access code shown in the terminal.');
      return;
    }
    setBusy(true);
    setLocalError(null);
    try {
      await onSubmit(trimmed);
    } catch (cause) {
      setLocalError(errorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const shownError = localError ?? error ?? null;

  return (
    <main className="login" id="main-content" ref={rootRef}>
      <div className="login__scene" aria-hidden="true">
        <CosmicScene />
      </div>

      <div className="login__stage">
        <div className="login__composition">
          <Watcher />

          <div className="login__card-shell">
            <form
              className="login__card"
              onSubmit={submit}
              aria-labelledby="login-title"
              aria-busy={busy}
              noValidate
            >
              <div className="login__card-sheen" aria-hidden="true" />

              <header className="login__head">
                <p className="login__eyebrow">
                  <span className="login__signal" aria-hidden="true" />
                  Local · single operator
                </p>
                <h1 id="login-title">
                  Sign in to <span className="login__brand">LayerAgent</span>
                </h1>
                <p className="login__hint" id="access-code-hint">
                  Enter the single-use code printed in the terminal. It expires after one use.
                </p>
              </header>

              <div className="login__field">
                <div className="login__label-row">
                  <label className="login__label" htmlFor="access-code">
                    Access code
                  </label>
                </div>
                <div className="login__input-shell">
                  <svg
                    className="login__key"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.6"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    aria-hidden="true"
                    focusable="false"
                  >
                    <circle cx="8.5" cy="8.5" r="4.5" />
                    <path d="m12 12 8 8" />
                    <path d="m16.5 16.5 2-2" />
                    <path d="m18.5 18.5 2-2" />
                  </svg>
                  <input
                    id="access-code"
                    name="access-code"
                    className="login__input"
                    value={code}
                    onChange={(event) => {
                      setCode(event.target.value);
                      if (localError) setLocalError(null);
                    }}
                    autoComplete="one-time-code"
                    autoCapitalize="characters"
                    autoCorrect="off"
                    spellCheck={false}
                    aria-describedby={
                      shownError ? 'access-code-hint access-code-error' : 'access-code-hint'
                    }
                    aria-invalid={shownError ? true : undefined}
                    autoFocus
                    required
                  />
                  <span className="login__input-glow" aria-hidden="true" />
                </div>
              </div>

              {shownError ? (
                <p className="login__error" id="access-code-error" role="alert">
                  <svg
                    className="login__error-icon"
                    viewBox="0 0 20 20"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.6"
                    strokeLinecap="round"
                    aria-hidden="true"
                    focusable="false"
                  >
                    <circle cx="10" cy="10" r="7.5" />
                    <path d="M10 6.4v4.4" />
                    <path d="M10 13.7h.01" />
                  </svg>
                  {shownError}
                </p>
              ) : null}

              <button type="submit" className="login__submit" disabled={busy}>
                <span className="login__submit-label">{busy ? 'Signing in…' : 'Sign in'}</span>
                {busy ? (
                  <span className="login__spinner" aria-hidden="true" />
                ) : (
                  <svg
                    className="login__submit-arrow"
                    viewBox="0 0 20 20"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="1.7"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    aria-hidden="true"
                    focusable="false"
                  >
                    <path d="M4 10h11" />
                    <path d="m11 6 4 4-4 4" />
                  </svg>
                )}
              </button>

              <p className="login__meta">
                <svg
                  className="login__lock"
                  viewBox="0 0 20 20"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.5"
                  strokeLinecap="round"
                  aria-hidden="true"
                  focusable="false"
                >
                  <rect x="4.5" y="8.5" width="11" height="8" rx="2" />
                  <path d="M7 8.5V6.8a3 3 0 0 1 6 0v1.7" />
                </svg>
                <span>HttpOnly session · stays on this machine</span>
              </p>
            </form>
          </div>
        </div>

        <div className="login__scroll-cue" aria-hidden="true">
          <span>Scroll to look closer</span>
          <span className="login__scroll-line" />
        </div>
      </div>

      <div className="login__runway" aria-hidden="true" />
    </main>
  );
}
