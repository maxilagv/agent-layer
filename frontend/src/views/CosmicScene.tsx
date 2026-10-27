/**
 * The full-screen WebGL space behind the sign-in card.
 *
 * This is a self-contained renderer: a procedural nebula with a gravitational
 * gateway, a star field, lens flare, bloom, chromatic aberration, film grain
 * and a vignette. It uses no external assets or libraries, so it still fits
 * the loopback surface's `default-src 'self'` policy.
 *
 * The effect is deliberately disposable: if WebGL2 is unavailable the canvas
 * hides itself and the CSS fallback behind it remains legible.
 */
import { useEffect, useRef } from 'react';

const VERTEX_SHADER = `#version 300 es
in vec2 a;
void main() {
  gl_Position = vec4(a, 0.0, 1.0);
}`;

const SCENE_SHADER = `#version 300 es
precision highp float;
uniform vec2 u_res; uniform float u_time; uniform vec2 u_pointer; uniform float u_scroll;
out vec4 outColor;
float hash21(vec2 p){p=fract(p*vec2(123.34,456.21));p+=dot(p,p+45.32);return fract(p.x*p.y);}
vec2 hash22(vec2 p){p=fract(p*vec2(123.34,456.21));p+=dot(p,p+45.32);return fract(vec2(p.x*p.y,p.x+p.y));}
float noise(vec2 p){vec2 i=floor(p),f=fract(p);f=f*f*(3.-2.*f);float a=hash21(i),b=hash21(i+vec2(1,0)),c=hash21(i+vec2(0,1)),d=hash21(i+vec2(1,1));return mix(mix(a,b,f.x),mix(c,d,f.x),f.y);}
float fbm(vec2 p){float v=0.,a=.5;mat2 m=mat2(.8,.6,-.6,.8);for(int i=0;i<5;i++){v+=a*noise(p);p=m*p*2.03+vec2(1.7,9.2);a*=.5;}return v;}
float starLayer(vec2 p,float scale,float seed,float thresh,float t,float speed){
  vec2 sp=p*scale+vec2(t*.004*speed,-t*.002*speed);
  vec2 cell=floor(sp); vec2 f=fract(sp);
  float h=hash21(cell+seed);
  if(h<thresh) return 0.;
  vec2 pos=hash22(cell+seed)*.72+.14;
  float d=length((f-pos)*vec2(1.,1.12));
  float tw=.55+.45*sin(t*(.45+h*1.7)+h*30.);
  float core=smoothstep(.010,.0,d);
  float glow=exp(-d*55.)*.4;
  return (core*1.7+glow)*tw*smoothstep(thresh,min(1.,thresh+.035),h);
}
float bokehLayer(vec2 p,float scale,float seed,float t,float speed){
  vec2 sp=p*scale+vec2(t*.003*speed,-t*.002*speed);
  vec2 cell=floor(sp); vec2 f=fract(sp);
  float h=hash21(cell+seed);
  if(h<.78) return 0.;
  vec2 pos=hash22(cell+seed)*.64+.18;
  float d=length((f-pos)*vec2(1.,1.05));
  float radius=.05+h*.09;
  float disc=smoothstep(radius,0.,d);
  return disc*disc*(.05+.05*sin(t*.2+h*20.));
}
void main(){
  vec2 uv=(gl_FragCoord.xy-.5*u_res)/u_res.y;
  uv+=u_pointer*.011;
  uv*=1.+u_scroll*.04;
  float t=u_time;
  vec3 col=vec3(.0016,.0024,.0075);
  col+=mix(vec3(.002,.006,.020),vec3(.014,.005,.024),smoothstep(-.7,.7,uv.y))*.85;
  // nebula veil, organic and asymmetric
  vec2 q=uv*1.65+vec2(t*.006,-t*.004);
  q+=vec2(fbm(q*.58+vec2(1.3,4.1)),fbm(q*.58+vec2(7.1,2.4)))*.82;
  float n=fbm(q*1.35+vec2(-t*.007,t*.003));
  float n2=fbm(q*2.7+vec2(3.2,8.4)+n*.95);
  float wisps=smoothstep(.35,.78,n)*smoothstep(.08,.72,n2);
  vec3 teal=vec3(.02,.22,.22), indigo=vec3(.05,.075,.32), plum=vec3(.22,.035,.20);
  col+=mix(indigo,teal,smoothstep(.2,.78,n2))*pow(wisps,1.25)*1.15;
  col+=plum*pow(smoothstep(.52,.9,n2),3.)*.55;
  // galactic band
  float bandCoord=uv.y*.82+uv.x*.57-.04;
  float band=exp(-pow(bandCoord/.25,2.));
  float dust=fbm(uv*3.4+vec2(4.0,1.2));
  col+=band*(.10+.26*dust)*mix(vec3(.02,.04,.13),vec3(.10,.035,.15),smoothstep(.2,.8,dust));
  // stars
  vec3 stars=vec3(0.);
  float density=.55+.45*band;
  stars+=starLayer(uv,9.,1.7,.958,t,1.)*vec3(.65,.84,1.);
  stars+=starLayer(uv,15.,8.1,.972,t,.7)*vec3(.9,.92,1.);
  stars+=starLayer(uv,25.,3.3,.980,t,.4)*vec3(1.,.84,.64);
  stars+=starLayer(uv,41.,12.7,.986,t,.25)*vec3(.8,.95,1.);
  col+=stars*density*1.35;
  // distant ringed planet, upper left
  vec2 pc=vec2(-.69,.51); float pr=length(uv-pc); float R=.078;
  if(pr<R){
    vec2 sp=(uv-pc)/R; float z=sqrt(max(.0,1.-dot(sp,sp))); vec3 nrm=vec3(sp,z);
    float diff=max(dot(nrm,normalize(vec3(.6,.5,.62))),0.);
    float limb=pow(1.-z,1.7);
    vec3 surf=vec3(.002,.004,.009)+vec3(.035,.065,.11)*diff;
    surf+=vec3(.08,.18,.28)*limb*.4;
    surf+=vec3(.02,.04,.07)*fbm(sp*5.+2.)*diff*.3;
    col=mix(col,surf,smoothstep(R+.002,R-.002,pr));
  }
  col+=vec3(.16,.38,.62)*exp(-abs(pr-R)/.006)*.14;
  // the eclipse gateway behind the avatar
  vec2 ec=vec2(-.005,.175);
  float er=length((uv-ec)*vec2(1.,1.06));
  float R2=.225;
  float ang=atan(uv.y-ec.y,uv.x-ec.x);
  float cn=fbm(vec2(ang*2.7+er*5.5, er*4.0-t*.07));
  // dark throat, with a faint breath of blue inside
  float inside=smoothstep(R2+.004,R2-.006,er);
  vec3 throat=vec3(.0015,.0030,.0080);
  throat+=vec3(.012,.028,.055)*smoothstep(R2,.0,er)*.55;
  throat+=vec3(.012,.006,.02)*fbm(uv*7.0+vec2(t*.02,t*.01))*smoothstep(R2,R2*.3,er)*.5;
  col=mix(col,throat,inside);
  // turbulent corona
  float streamers=pow(.5+.5*sin(ang*7.0+cn*5.0+t*.18+sin(ang*3.0)*.8),4.0);
  float corona=exp(-max(er-R2,0.)/.105)*(.28+.72*streamers);
  corona*=smoothstep(R2+.002,R2+.012,er)*(1.-smoothstep(R2+.34,R2+.62,er));
  vec3 coronaCol=mix(vec3(.10,.62,.95),vec3(1.,.32,.64),.5+.5*sin(ang*1.6+t*.09));
  coronaCol=mix(vec3(1.,.93,.86),coronaCol,smoothstep(R2+.02,R2+.08,er));
  col+=coronaCol*corona*.95;
  // photon rim, plus a broken outer arc
  float rim=exp(-pow((er-R2)/.0065,2.));
  float rim2=exp(-pow((er-R2-.018)/.0028,2.))*(.25+.75*smoothstep(.25,.7,cn));
  col+=vec3(1.,.95,.9)*rim*1.15;
  col+=vec3(.35,.78,1.)*rim2*.45;
  // faint accretion arcs
  float arc=exp(-pow((er-(R2+.055+ .008*sin(ang*3.+t*.1)))/.0025,2.))*(.15+.85*streamers);
  col+=vec3(.35,.62,1.)*arc*.24;
  // radial light rays, very soft
  float rays=pow(.5+.5*cos(ang*7.+sin(t*.12)*.8+cn*1.2),7.)*exp(-abs(er-R2)/.32)*.05;
  col+=vec3(.35,.68,1.)*rays;
  float plume=exp(-length((uv-vec2(.34,.12))*vec2(.72,1.25))/.22)*smoothstep(.2,.8,fbm(uv*4.5+vec2(t*.02,1.2)));
  col+=vec3(.08,.2,.38)*plume*.22;
  // lens flare from the gateway
  float hfl=exp(-abs(uv.y-ec.y)*95.)*exp(-abs(uv.x-ec.x)*2.3)*.22;
  float vfl=exp(-abs(uv.x-ec.x)*160.)*exp(-abs(uv.y-ec.y)*5.5)*.10;
  col+=vec3(.45,.72,1.)*(hfl+vfl);
  // bright star, upper right
  vec2 sc=vec2(.58,.50);
  float sd=length((uv-sc)*vec2(1.,1.12));
  col+=vec3(.65,.86,1.)*(exp(-sd*28.)*.65+exp(-sd*8.)*.10);
  col+=vec3(.45,.7,1.)*(exp(-abs(uv.x-sc.x)*130.)*exp(-abs(uv.y-sc.y)*8.)+exp(-abs(uv.y-sc.y)*130.)*exp(-abs(uv.x-sc.x)*8.))*.25;
  // foreground bokeh
  float bok=bokehLayer(uv,4.0,6.7,t,.4)+bokehLayer(uv,6.8,3.1,t,.25);
  col+=vec3(.42,.66,1.)*bok;
  // low aurora behind the card
  float aur=pow(.5+.5*sin(uv.x*5.5+t*.07+sin(uv.x*2.2-t*.03)*1.5),5.);
  aur*=smoothstep(-.25,-.65,uv.y)*smoothstep(-1.0,-.55,uv.y);
  col+=mix(vec3(.0,.14,.20),vec3(.09,.02,.16),smoothstep(-.7,-.25,uv.y))*aur*.16;
  col+=vec3(.008,.02,.05)*exp(-length(uv)*1.0)*.25;
  outColor=vec4(max(col,0.),1.);
}`;

const BRIGHT_SHADER = `#version 300 es
precision highp float; uniform sampler2D u_scene; uniform vec2 u_res; out vec4 outColor;
void main(){vec2 uv=gl_FragCoord.xy/u_res; vec3 c=texture(u_scene,uv).rgb; float l=dot(c,vec3(.2126,.7152,.0722)); outColor=vec4(c*smoothstep(.18,.58,l),1.);}`;

const BLUR_SHADER = `#version 300 es
precision highp float; uniform sampler2D u_scene; uniform vec2 u_res; uniform vec2 u_dir; out vec4 outColor;
void main(){vec2 uv=gl_FragCoord.xy/u_res; vec2 s=u_dir/u_res; vec3 c=texture(u_scene,uv).rgb*.227; c+=texture(u_scene,uv+s).rgb*.194; c+=texture(u_scene,uv-s).rgb*.194; c+=texture(u_scene,uv+s*2.).rgb*.121; c+=texture(u_scene,uv-s*2.).rgb*.121; c+=texture(u_scene,uv+s*3.).rgb*.054; c+=texture(u_scene,uv-s*3.).rgb*.054; c+=texture(u_scene,uv+s*4.).rgb*.016; c+=texture(u_scene,uv-s*4.).rgb*.016; outColor=vec4(c,1.);}`;

const COMPOSITE_SHADER = `#version 300 es
precision highp float; uniform sampler2D u_scene; uniform sampler2D u_bloom; uniform vec2 u_res; uniform float u_time; out vec4 outColor;
float hash(vec2 p){p=fract(p*vec2(123.34,456.21));p+=dot(p,p+45.32);return fract(p.x*p.y);}
void main(){vec2 uv=gl_FragCoord.xy/u_res; vec2 q=uv-.5; float ca=.0012+dot(q,q)*.007; vec3 c; c.r=texture(u_scene,uv+q*ca).r; c.g=texture(u_scene,uv).g; c.b=texture(u_scene,uv-q*ca).b; vec3 b=texture(u_bloom,uv).rgb; c+=b*vec3(.95,.86,1.08)*1.15; c=1.-exp(-c*1.65); float l=dot(c,vec3(.2126,.7152,.0722)); c=mix(vec3(l),c,1.2); float vig=smoothstep(1.3,.14,length(q*vec2(1.0,1.08))); c*=mix(.5,1.,vig); c+=(hash(gl_FragCoord.xy+u_time*90.)-.5)*.019; c+=(hash(gl_FragCoord.xy*.7)-.5)/255.; outColor=vec4(max(c,0.),1.);}`;

const FULLSCREEN_TRIANGLE = new Float32Array([-1, -1, 3, -1, -1, 3]);

type RenderTarget = {
  texture: WebGLTexture;
  framebuffer: WebGLFramebuffer;
  width: number;
  height: number;
};

type Layout = {
  width: number;
  height: number;
  scene: RenderTarget;
  bright: RenderTarget;
  blur: RenderTarget;
};

type SceneUniforms = {
  resolution: WebGLUniformLocation | null;
  time: WebGLUniformLocation | null;
  pointer: WebGLUniformLocation | null;
  scroll: WebGLUniformLocation | null;
};

type TextureUniforms = {
  scene: WebGLUniformLocation | null;
  resolution: WebGLUniformLocation | null;
};

type BlurUniforms = TextureUniforms & {
  direction: WebGLUniformLocation | null;
};

type CompositeUniforms = TextureUniforms & {
  bloom: WebGLUniformLocation | null;
  time: WebGLUniformLocation | null;
};

function compileShader(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader {
  const shader = gl.createShader(type);
  if (!shader) throw new Error('Unable to allocate a WebGL shader.');
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const info = gl.getShaderInfoLog(shader) ?? 'unknown shader error';
    gl.deleteShader(shader);
    throw new Error(info);
  }
  return shader;
}

function createProgram(
  gl: WebGL2RenderingContext,
  vertexSource: string,
  fragmentSource: string,
): WebGLProgram {
  const vertex = compileShader(gl, gl.VERTEX_SHADER, vertexSource);
  const fragment = compileShader(gl, gl.FRAGMENT_SHADER, fragmentSource);
  const program = gl.createProgram();
  if (!program) throw new Error('Unable to allocate a WebGL program.');
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.bindAttribLocation(program, 0, 'a');
  gl.linkProgram(program);
  gl.deleteShader(vertex);
  gl.deleteShader(fragment);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const info = gl.getProgramInfoLog(program) ?? 'unknown program link error';
    gl.deleteProgram(program);
    throw new Error(info);
  }
  return program;
}

function createTarget(gl: WebGL2RenderingContext, width: number, height: number): RenderTarget {
  const texture = gl.createTexture();
  const framebuffer = gl.createFramebuffer();
  if (!texture || !framebuffer) throw new Error('Unable to allocate a WebGL render target.');

  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);

  gl.bindFramebuffer(gl.FRAMEBUFFER, framebuffer);
  gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, texture, 0);
  if (gl.checkFramebufferStatus(gl.FRAMEBUFFER) !== gl.FRAMEBUFFER_COMPLETE) {
    gl.deleteTexture(texture);
    gl.deleteFramebuffer(framebuffer);
    throw new Error('The WebGL render target is incomplete.');
  }

  return { texture, framebuffer, width, height };
}

function disposeTarget(gl: WebGL2RenderingContext, target: RenderTarget): void {
  gl.deleteTexture(target.texture);
  gl.deleteFramebuffer(target.framebuffer);
}

export function CosmicScene({ className }: { className?: string }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    // jsdom has no WebGL2. Do not call getContext there: it only logs noise.
    if (
      typeof window.WebGL2RenderingContext === 'undefined' ||
      typeof canvas.getContext !== 'function'
    ) {
      canvas.style.display = 'none';
      return;
    }

    let gl: WebGL2RenderingContext | null = null;
    try {
      gl = canvas.getContext('webgl2', {
        alpha: false,
        antialias: false,
        depth: false,
        powerPreference: 'high-performance',
        preserveDrawingBuffer: false,
      });
    } catch {
      gl = null;
    }
    if (!gl) {
      canvas.style.display = 'none';
      return;
    }

    const context = gl;
    const reduceMotion =
      typeof window.matchMedia === 'function' &&
      window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    let sceneProgram: WebGLProgram | null = null;
    let brightProgram: WebGLProgram | null = null;
    let blurProgram: WebGLProgram | null = null;
    let compositeProgram: WebGLProgram | null = null;
    let vertexArray: WebGLVertexArrayObject | null = null;
    let buffer: WebGLBuffer | null = null;
    let layout: Layout | null = null;
    let frameId = 0;
    let started = 0;
    let pointerX = 0;
    let pointerY = 0;
    let targetX = 0;
    let targetY = 0;
    let scroll = 0;

    const handleFailure = () => {
      canvas.style.display = 'none';
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
        frameId = 0;
      }
    };

    try {
      sceneProgram = createProgram(context, VERTEX_SHADER, SCENE_SHADER);
      brightProgram = createProgram(context, VERTEX_SHADER, BRIGHT_SHADER);
      blurProgram = createProgram(context, VERTEX_SHADER, BLUR_SHADER);
      compositeProgram = createProgram(context, VERTEX_SHADER, COMPOSITE_SHADER);

      vertexArray = context.createVertexArray();
      buffer = context.createBuffer();
      if (!vertexArray || !buffer) throw new Error('Unable to allocate WebGL geometry.');
      context.bindVertexArray(vertexArray);
      context.bindBuffer(context.ARRAY_BUFFER, buffer);
      context.bufferData(context.ARRAY_BUFFER, FULLSCREEN_TRIANGLE, context.STATIC_DRAW);
      context.enableVertexAttribArray(0);
      context.vertexAttribPointer(0, 2, context.FLOAT, false, 0, 0);
    } catch (cause) {
      console.warn('Cosmic background disabled:', cause);
      handleFailure();
      return;
    }

    const sceneUniforms: SceneUniforms = {
      resolution: context.getUniformLocation(sceneProgram, 'u_res'),
      time: context.getUniformLocation(sceneProgram, 'u_time'),
      pointer: context.getUniformLocation(sceneProgram, 'u_pointer'),
      scroll: context.getUniformLocation(sceneProgram, 'u_scroll'),
    };
    const brightUniforms: TextureUniforms = {
      scene: context.getUniformLocation(brightProgram, 'u_scene'),
      resolution: context.getUniformLocation(brightProgram, 'u_res'),
    };
    const blurUniforms: BlurUniforms = {
      scene: context.getUniformLocation(blurProgram, 'u_scene'),
      resolution: context.getUniformLocation(blurProgram, 'u_res'),
      direction: context.getUniformLocation(blurProgram, 'u_dir'),
    };
    const compositeUniforms: CompositeUniforms = {
      scene: context.getUniformLocation(compositeProgram, 'u_scene'),
      bloom: context.getUniformLocation(compositeProgram, 'u_bloom'),
      resolution: context.getUniformLocation(compositeProgram, 'u_res'),
      time: context.getUniformLocation(compositeProgram, 'u_time'),
    };

    const resize = () => {
      const cssWidth = canvas.clientWidth || window.innerWidth;
      const cssHeight = canvas.clientHeight || window.innerHeight;
      const maxRatio = window.innerWidth < 640 ? 1.25 : 1.5;
      const ratio = Math.min(window.devicePixelRatio || 1, maxRatio);
      const width = Math.max(1, Math.round(cssWidth * ratio));
      const height = Math.max(1, Math.round(cssHeight * ratio));
      if (layout && layout.width === width && layout.height === height) return;

      if (layout) {
        disposeTarget(context, layout.scene);
        disposeTarget(context, layout.bright);
        disposeTarget(context, layout.blur);
      }

      canvas.width = width;
      canvas.height = height;
      const halfWidth = Math.max(1, Math.floor(width / 2));
      const halfHeight = Math.max(1, Math.floor(height / 2));
      layout = {
        width,
        height,
        scene: createTarget(context, width, height),
        bright: createTarget(context, halfWidth, halfHeight),
        blur: createTarget(context, halfWidth, halfHeight),
      };
    };

    const draw = (time: number) => {
      if (!layout || !sceneProgram || !brightProgram || !blurProgram || !compositeProgram) return;

      context.bindVertexArray(vertexArray);
      context.disable(context.BLEND);

      context.bindFramebuffer(context.FRAMEBUFFER, layout.scene.framebuffer);
      context.viewport(0, 0, layout.width, layout.height);
      context.useProgram(sceneProgram);
      context.uniform2f(sceneUniforms.resolution, layout.width, layout.height);
      context.uniform1f(sceneUniforms.time, time);
      context.uniform2f(sceneUniforms.pointer, pointerX, pointerY);
      context.uniform1f(sceneUniforms.scroll, scroll);
      context.drawArrays(context.TRIANGLES, 0, 3);

      context.bindFramebuffer(context.FRAMEBUFFER, layout.bright.framebuffer);
      context.viewport(0, 0, layout.bright.width, layout.bright.height);
      context.useProgram(brightProgram);
      context.activeTexture(context.TEXTURE0);
      context.bindTexture(context.TEXTURE_2D, layout.scene.texture);
      context.uniform1i(brightUniforms.scene, 0);
      context.uniform2f(brightUniforms.resolution, layout.bright.width, layout.bright.height);
      context.drawArrays(context.TRIANGLES, 0, 3);

      context.bindFramebuffer(context.FRAMEBUFFER, layout.blur.framebuffer);
      context.viewport(0, 0, layout.blur.width, layout.blur.height);
      context.useProgram(blurProgram);
      context.activeTexture(context.TEXTURE0);
      context.bindTexture(context.TEXTURE_2D, layout.bright.texture);
      context.uniform1i(blurUniforms.scene, 0);
      context.uniform2f(blurUniforms.resolution, layout.blur.width, layout.blur.height);
      context.uniform2f(blurUniforms.direction, 1, 0);
      context.drawArrays(context.TRIANGLES, 0, 3);

      context.bindFramebuffer(context.FRAMEBUFFER, layout.bright.framebuffer);
      context.viewport(0, 0, layout.bright.width, layout.bright.height);
      context.useProgram(blurProgram);
      context.activeTexture(context.TEXTURE0);
      context.bindTexture(context.TEXTURE_2D, layout.blur.texture);
      context.uniform1i(blurUniforms.scene, 0);
      context.uniform2f(blurUniforms.resolution, layout.bright.width, layout.bright.height);
      context.uniform2f(blurUniforms.direction, 0, 1);
      context.drawArrays(context.TRIANGLES, 0, 3);

      context.bindFramebuffer(context.FRAMEBUFFER, null);
      context.viewport(0, 0, layout.width, layout.height);
      context.useProgram(compositeProgram);
      context.activeTexture(context.TEXTURE0);
      context.bindTexture(context.TEXTURE_2D, layout.scene.texture);
      context.uniform1i(compositeUniforms.scene, 0);
      context.activeTexture(context.TEXTURE1);
      context.bindTexture(context.TEXTURE_2D, layout.bright.texture);
      context.uniform1i(compositeUniforms.bloom, 1);
      context.uniform2f(compositeUniforms.resolution, layout.width, layout.height);
      context.uniform1f(compositeUniforms.time, time);
      context.drawArrays(context.TRIANGLES, 0, 3);
    };

    const onPointerMove = (event: PointerEvent) => {
      targetX = (event.clientX / Math.max(1, window.innerWidth) - 0.5) * 2;
      targetY = -(event.clientY / Math.max(1, window.innerHeight) - 0.5) * 2;
    };
    const onScroll = () => {
      scroll = Math.min(1, Math.max(0, window.scrollY / Math.max(1, window.innerHeight * 0.7)));
    };
    const onResize = () => {
      try {
        resize();
        if (reduceMotion) draw(4.2);
      } catch (cause) {
        console.warn('Cosmic background resize failed:', cause);
        handleFailure();
      }
    };
    const onVisibilityChange = () => {
      if (document.hidden) {
        if (frameId !== 0) {
          window.cancelAnimationFrame(frameId);
          frameId = 0;
        }
      } else if (!reduceMotion && frameId === 0) {
        frameId = window.requestAnimationFrame(tick);
      }
    };
    const onContextLost = (event: Event) => {
      event.preventDefault();
      handleFailure();
    };

    const tick = (now: number) => {
      if (document.hidden) {
        frameId = 0;
        return;
      }
      pointerX += (targetX - pointerX) * 0.035;
      pointerY += (targetY - pointerY) * 0.035;
      draw((now - started) / 1000);
      frameId = window.requestAnimationFrame(tick);
    };

    try {
      resize();
    } catch (cause) {
      console.warn('Cosmic background could not allocate render targets:', cause);
      handleFailure();
      return;
    }

    started = performance.now();
    if (reduceMotion) {
      draw(4.2);
    } else {
      onScroll();
      frameId = window.requestAnimationFrame(tick);
      window.addEventListener('pointermove', onPointerMove, { passive: true });
      window.addEventListener('scroll', onScroll, { passive: true });
    }
    window.addEventListener('resize', onResize, { passive: true });
    document.addEventListener('visibilitychange', onVisibilityChange);
    canvas.addEventListener('webglcontextlost', onContextLost);

    return () => {
      if (frameId !== 0) window.cancelAnimationFrame(frameId);
      window.removeEventListener('pointermove', onPointerMove);
      window.removeEventListener('scroll', onScroll);
      window.removeEventListener('resize', onResize);
      document.removeEventListener('visibilitychange', onVisibilityChange);
      canvas.removeEventListener('webglcontextlost', onContextLost);

      if (layout) {
        disposeTarget(context, layout.scene);
        disposeTarget(context, layout.bright);
        disposeTarget(context, layout.blur);
      }
      if (buffer) context.deleteBuffer(buffer);
      if (vertexArray) context.deleteVertexArray(vertexArray);
      if (sceneProgram) context.deleteProgram(sceneProgram);
      if (brightProgram) context.deleteProgram(brightProgram);
      if (blurProgram) context.deleteProgram(blurProgram);
      if (compositeProgram) context.deleteProgram(compositeProgram);
    };
  }, []);

  return <canvas ref={canvasRef} className={`login__cosmos ${className ?? ''}`.trim()} aria-hidden="true" />;
}
