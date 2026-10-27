/**
 * The watcher above the sign-in card.
 *
 * This is an original vector mark inspired by the mascot: a pointed hood, a
 * dark void, two cyan eyes, layered icy feathers and a small cloud base. It
 * deliberately does not render the mascot bitmap, and it is decorative only.
 */
export function Watcher() {
  return (
    <div className="login__watcher" aria-hidden="true">
      <div className="login__watcher-halo" />
      <div className="login__watcher-float">
        <svg
          className="login__hood"
          viewBox="0 0 760 460"
          role="presentation"
          focusable="false"
        >
          <defs>
            <linearGradient id="login-body-gradient" x1="0.2" y1="0" x2="0.8" y2="1">
              <stop offset="0" stopColor="#0b1b44" />
              <stop offset="0.42" stopColor="#06102b" />
              <stop offset="1" stopColor="#02050f" />
            </linearGradient>
            <linearGradient id="login-wing-gradient" x1="0" y1="1" x2="0.9" y2="0">
              <stop offset="0" stopColor="#07132e" />
              <stop offset="0.25" stopColor="#143f82" stopOpacity="0.92" />
              <stop offset="0.58" stopColor="#4f93df" stopOpacity="0.55" />
              <stop offset="0.84" stopColor="#b5e3ff" stopOpacity="0.42" />
              <stop offset="1" stopColor="#eafaff" stopOpacity="0.55" />
            </linearGradient>
            <linearGradient id="login-rim-gradient" x1="0" y1="0" x2="1" y2="1">
              <stop offset="0" stopColor="#c3ecff" stopOpacity="0.68" />
              <stop offset="0.45" stopColor="#5b9dff" stopOpacity="0.3" />
              <stop offset="1" stopColor="#5b9dff" stopOpacity="0" />
            </linearGradient>
            <linearGradient id="login-feather-gradient" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0" stopColor="#d5f2ff" stopOpacity="0.7" />
              <stop offset="0.5" stopColor="#74b6ff" stopOpacity="0.3" />
              <stop offset="1" stopColor="#4f8dff" stopOpacity="0" />
            </linearGradient>
            <radialGradient id="login-eye-gradient" cx="0.5" cy="0.42" r="0.7">
              <stop offset="0" stopColor="#ffffff" />
              <stop offset="0.35" stopColor="#dcfaff" />
              <stop offset="0.72" stopColor="#65caff" />
              <stop offset="1" stopColor="#2362ff" />
            </radialGradient>
            <radialGradient id="login-cloud-puff" cx="0.5" cy="0.32" r="0.68">
              <stop offset="0" stopColor="#2b5f9f" stopOpacity="0.36" />
              <stop offset="0.52" stopColor="#0b2044" stopOpacity="0.58" />
              <stop offset="1" stopColor="#030814" stopOpacity="0" />
            </radialGradient>
          </defs>

          <g className="login__cloud" fill="url(#login-cloud-puff)">
            <ellipse cx="152" cy="424" rx="54" ry="34" />
            <ellipse cx="214" cy="414" rx="52" ry="38" />
            <ellipse cx="284" cy="422" rx="60" ry="40" />
            <ellipse cx="362" cy="416" rx="58" ry="42" />
            <ellipse cx="440" cy="420" rx="58" ry="40" />
            <ellipse cx="512" cy="414" rx="52" ry="38" />
            <ellipse cx="578" cy="422" rx="58" ry="36" />
            <ellipse cx="632" cy="428" rx="44" ry="30" />
          </g>

          <g className="login__wing" fill="url(#login-wing-gradient)" stroke="url(#login-feather-gradient)" strokeWidth="1.05" strokeOpacity="0.75">
            <path d="M498 236C522 172 552 94 592 20 582 102 562 184 530 242Z" />
            <path d="M503 244C538 196 596 124 654 56 622 138 588 210 542 252Z" />
            <path d="M508 252C562 214 636 156 704 102 664 182 618 238 552 262Z" />
            <path d="M512 262C582 240 670 194 740 152 696 228 644 274 560 274Z" />
            <path d="M516 272C592 264 686 240 756 208 714 278 658 308 566 286Z" />
            <path d="M516 282C592 290 688 286 748 264 706 328 650 348 562 300Z" />
            <path d="M512 292C582 314 668 330 716 318 680 374 624 384 552 316Z" />
            <path d="M505 302C566 334 636 364 666 368 630 410 578 404 532 334Z" />
            <path d="M498 308C540 344 582 384 602 408 562 404 520 374 494 342Z" />
          </g>
          <g className="login__feather-lines" fill="none" stroke="#d5f2ff" strokeOpacity="0.36" strokeWidth="0.9">
            <path d="M500 232C524 168 554 92 594 18" />
            <path d="M505 240C540 192 598 122 656 54" />
            <path d="M510 248C564 210 638 154 706 100" />
            <path d="M514 258C584 236 672 192 742 150" />
            <path d="M518 268C594 260 688 238 758 206" />
            <path d="M518 278C594 286 690 284 750 262" />
            <path d="M514 288C584 310 670 328 718 316" />
            <path d="M507 298C568 330 638 362 668 366" />
            <path d="M500 304C542 340 584 380 604 404" />
          </g>

          <g className="login__wing" transform="translate(760 0) scale(-1 1)" fill="url(#login-wing-gradient)" stroke="url(#login-feather-gradient)" strokeWidth="1.05" strokeOpacity="0.75">
            <path d="M498 236C522 172 552 94 592 20 582 102 562 184 530 242Z" />
            <path d="M503 244C538 196 596 124 654 56 622 138 588 210 542 252Z" />
            <path d="M508 252C562 214 636 156 704 102 664 182 618 238 552 262Z" />
            <path d="M512 262C582 240 670 194 740 152 696 228 644 274 560 274Z" />
            <path d="M516 272C592 264 686 240 756 208 714 278 658 308 566 286Z" />
            <path d="M516 282C592 290 688 286 748 264 706 328 650 348 562 300Z" />
            <path d="M512 292C582 314 668 330 716 318 680 374 624 384 552 316Z" />
            <path d="M505 302C566 334 636 364 666 368 630 410 578 404 532 334Z" />
            <path d="M498 308C540 344 582 384 602 408 562 404 520 374 494 342Z" />
          </g>
          <g className="login__feather-lines" transform="translate(760 0) scale(-1 1)" fill="none" stroke="#d5f2ff" strokeOpacity="0.36" strokeWidth="0.9">
            <path d="M500 232C524 168 554 92 594 18" />
            <path d="M505 240C540 192 598 122 656 54" />
            <path d="M510 248C564 210 638 154 706 100" />
            <path d="M514 258C584 236 672 192 742 150" />
            <path d="M518 268C594 260 688 238 758 206" />
            <path d="M518 278C594 286 690 284 750 262" />
            <path d="M514 288C584 310 670 328 718 316" />
            <path d="M507 298C568 330 638 362 668 366" />
            <path d="M500 304C542 340 584 380 604 404" />
          </g>

          <path className="login__body" d="M380 252c-54 0-92 34-104 86-8 34-20 56-42 74 32 12 76 18 146 18s114-6 146-18c-22-18-34-40-42-74-12-52-50-86-104-86Z" fill="#06102b" stroke="url(#login-rim-gradient)" strokeWidth="1.6" />
          <g fill="#07132e" stroke="url(#login-feather-gradient)" strokeWidth="1.1" strokeOpacity="0.55">
            <path d="M292 296c-28 26-38 58-24 84 18-18 40-36 66-46Z" />
            <path d="M468 296c28 26 38 58 24 84-18-18-40-36-66-46Z" />
          </g>

          <path className="login__hood-body" d="M380 32c-40 20-72 50-88 86-18 38-20 82-10 116 10 32 34 54 64 66h68c30-12 54-34 64-66 10-34 8-78-10-116-16-36-48-66-88-86Z" fill="url(#login-body-gradient)" stroke="url(#login-rim-gradient)" strokeWidth="2" />
          <path className="login__face" d="M380 130c-32 0-52 26-56 62-4 35 10 70 32 88h48c22-18 36-53 32-88-4-36-24-62-56-62Z" fill="#01030a" stroke="url(#login-rim-gradient)" strokeWidth="2.4" />
          <path d="M318 180c18-40 42-56 62-56s44 16 62 56" fill="none" stroke="#9fd9ff" strokeOpacity="0.16" strokeWidth="1.3" />
          <path d="M316 174c18-38 42-54 64-54s46 16 64 54c-20-16-42-24-64-24s-44 8-64 24Z" fill="#01030a" fillOpacity="0.92" />

          <g className="login__eye login__eye--left">
            <ellipse cx="350" cy="202" rx="27" ry="8.4" transform="rotate(-8 350 202)" fill="url(#login-eye-gradient)" />
            <ellipse className="login__pupil" cx="350" cy="202" rx="5.2" ry="6.6" fill="#03101c" />
            <ellipse cx="353" cy="199" rx="1.6" ry="1.8" fill="#ffffff" fillOpacity="0.92" />
          </g>
          <g className="login__eye login__eye--right">
            <ellipse cx="410" cy="202" rx="27" ry="8.4" transform="rotate(8 410 202)" fill="url(#login-eye-gradient)" />
            <ellipse className="login__pupil" cx="410" cy="202" rx="5.2" ry="6.6" fill="#03101c" />
            <ellipse cx="413" cy="199" rx="1.6" ry="1.8" fill="#ffffff" fillOpacity="0.92" />
          </g>

          <g fill="none" stroke="#7bb7ff" strokeOpacity="0.18" strokeWidth="1.3">
            <path d="M380 38c-6 40-7 78-3 112" />
            <path d="M316 118c-10 40-12 78-4 116" />
            <path d="M444 118c10 40 12 78 4 116" />
          </g>

          <path className="login__pendant" d="M380 330l8 12-8 15-8-15Z" fill="#9fe8ff" />
          <path className="login__cowl" d="M200 470c34-84 94-126 180-126s146 42 180 126Z" fill="#06102b" stroke="url(#login-rim-gradient)" strokeWidth="1.4" />

          <g className="login__sparkles" fill="#dff6ff">
            <path className="login__spark login__spark--a" d="M0-5 1.1-1.1 5 0 1.1 1.1 0 5-1.1 1.1-5 0-1.1-1.1Z" transform="translate(188 152)" />
            <path className="login__spark login__spark--b" d="M0-4 0.9-0.9 4 0 0.9 0.9 0 4-0.9 0.9-4 0-0.9-0.9Z" transform="translate(560 84)" />
            <path className="login__spark login__spark--c" d="M0-6 1.3-1.3 6 0 1.3 1.3 0 6-1.3 1.3-6 0-1.3-1.3Z" transform="translate(690 252)" />
            <path className="login__spark login__spark--d" d="M0-3.5 0.8-0.8 3.5 0 0.8 0.8 0 3.5-0.8 0.8-3.5 0-0.8-0.8Z" transform="translate(94 290)" />
            <path className="login__spark login__spark--e" d="M0-4.5 1-1 4.5 0 1 1 0 4.5-1 1-4.5 0-1-1Z" transform="translate(636 330)" />
          </g>
        </svg>
      </div>
    </div>
  );
}
