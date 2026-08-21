import LeaderLine, {
  type SocketType,
  type AnchorAttachment,
  type LeaderLine as LeaderLineCls
} from "@aquascope/leader-line";
import { Decoration, type EditorView, WidgetType } from "@codemirror/view";
import { library } from "@fortawesome/fontawesome-svg-core";
import {
  faBinoculars,
  faStepBackward,
  faStepForward
} from "@fortawesome/free-solid-svg-icons";
import classNames from "classnames";
import _ from "lodash";
import React, {
  type CSSProperties,
  useContext,
  useEffect,
  useRef,
  useState
} from "react";
import ReactDOM from "react-dom/client";

import type {
  Abbreviated,
  CharRange,
  InterpAnnotations,
  MFrame,
  MHeap,
  MLocal,
  MPathSegment,
  MStack,
  MStep,
  MTrace,
  MUndefinedBehavior,
  MValue
} from "../types.js";
import {
  evenlySpaceAround,
  linecolToPosition,
  makeDecorationField
} from "./misc.js";

library.add(faBinoculars, faStepBackward, faStepForward);

const DEBUG: boolean = false;

export interface InterpreterConfig {
  horizontal?: boolean;
  concreteTypes?: boolean;
  hideCode?: boolean;
  interpreterControls?: boolean;
}

let ConfigContext = React.createContext<InterpreterConfig>({});
let CodeContext = React.createContext<EditorView | undefined>(undefined);
let PathContext = React.createContext<string[]>([]);
let ErrorContext = React.createContext<MUndefinedBehavior | undefined>(
  undefined
);

// The moves that land somewhere *inside* the value being rendered, each one
// relative to it: `[]` means this value itself was moved, `[Field(0)]` means
// its first field was. A local whose `name` field was moved starts its value
// off with `[[Field(0)]]`, and the struct's field cell for index 0 receives
// `[[]]` -- that cell is the one that gets shaded.
let MovedPathsContext = React.createContext<MPathSegment[][]>([]);

let sameSegment = (a: MPathSegment, b: MPathSegment): boolean =>
  a.type === "Subslice" && b.type === "Subslice"
    ? a.value[0] === b.value[0] && a.value[1] === b.value[1]
    : a.type === b.type && a.value === b.value;

/// Routes the pending moved paths one level down, into the child reached by
/// `segment`. The child is moved if a path ends exactly at it.
let useMovedPaths = (
  segment: MPathSegment
): { moved: boolean; paths: MPathSegment[][] } => {
  let paths = useContext(MovedPathsContext);
  let descended = paths
    .filter(path => path.length > 0 && sameSegment(path[0], segment))
    .map(path => path.slice(1));
  return {
    moved: descended.some(path => path.length === 0),
    paths: descended.filter(path => path.length > 0)
  };
};

/// One child of a composite value -- a struct field, a tuple element, an array
/// element. Shaded when that child, rather than the value containing it, is
/// what was moved.
///
/// Given a `label`, this is a named field and renders the whole labelled row,
/// shading included, so that a moved-out field reads the way a moved-out local
/// does: the name leaves with the value.
let SubvalueView = ({
  segment,
  path,
  value,
  label,
  element: Element = "td",
  connector,
  children
}: {
  segment: MPathSegment;
  path: string[];
  value?: MValue;
  label?: string;
  element?: "td" | "span";
  connector?: string;
  children?: React.ReactNode;
}) => {
  let { moved, paths } = useMovedPaths(segment);
  // The shading goes on whichever element covers the name as well as the
  // value. Putting it on both would stack the two opacities and leave the
  // field almost invisible.
  let onRow = label !== undefined;
  let cell = (
    <Element
      className={classNames(path.join("-"), { moved: moved && !onRow })}
      data-connector={connector}
    >
      <PathContext.Provider value={path}>
        <MovedPathsContext.Provider value={paths}>
          {children ?? <ValueView value={value!} />}
        </MovedPathsContext.Provider>
      </PathContext.Provider>
    </Element>
  );

  if (!onRow) return cell;

  return (
    <tr className={classNames({ moved })}>
      <td>{label}</td>
      {cell}
    </tr>
  );
};

let codeRange = (view: EditorView, range: CharRange) => {
  let start = linecolToPosition(range.start, view.state.doc);
  let end = linecolToPosition(range.end, view.state.doc);
  return view.state.doc.sliceString(start, end);
};

let AbbreviatedView = ({ value }: { value: Abbreviated<MValue> }) => {
  let pathCtx = useContext(PathContext);
  let IndexedContainer: React.FC<
    React.PropsWithChildren<{ index: number }>
  > = ({ children, index }) => (
    <SubvalueView
      segment={{ type: "Index", value: index }}
      path={[...pathCtx, "index", index.toString()]}
      connector="bottom"
    >
      {children}
    </SubvalueView>
  );

  // TODO: handle indexes into abbreviated + end regions
  return (
    <table className="array">
      <tbody>
        <tr>
          {value.type === "All" ? (
            value.value.map((el, i) => (
              <IndexedContainer key={i} index={i}>
                <ValueView value={el} />
              </IndexedContainer>
            ))
          ) : (
            <>
              {value.value[0].map((el, i) => (
                <IndexedContainer key={i} index={i}>
                  <ValueView value={el} />
                </IndexedContainer>
              ))}
              <td>...</td>
              <IndexedContainer index={100}>
                <ValueView value={value.value[1]} />
              </IndexedContainer>
            </>
          )}
        </tr>
      </tbody>
    </table>
  );
};

type MValueAdt = MValue & { type: "Adt" };
type MAdt = MValueAdt["value"];

type MValuePointer = MValue & { type: "Pointer" };
type MPointer = MValuePointer["value"];

let read_field = (v: MAdt, k: string): MAdt => {
  let field = v.fields.find(([k2]) => k === k2);
  if (!field) {
    let v_pretty = JSON.stringify(v, undefined, 2);
    throw new Error(`Could not find field "${k}" in struct: ${v_pretty}`);
  }
  return (field[1] as MValueAdt).value;
};

let read_unique = (unique: MAdt): MAdt => {
  let non_null = read_field(unique, "pointer");
  return non_null;
};

let read_vec = (vec: MAdt): MAdt => {
  let raw_vec = read_field(vec, "buf");
  let raw_vec_inner = read_field(raw_vec, "inner");
  let unique = read_field(raw_vec_inner, "ptr");
  return read_unique(unique);
};

let specialPtr = (value: MAdt): MValue | undefined => {
  if (value.alloc_kind === null) return;

  let alloc_type = value.alloc_kind.type;
  let non_null: MAdt;
  if (alloc_type === "String") {
    let vec = read_field(value, "vec");
    non_null = read_vec(vec);
  } else if (alloc_type === "Vec") {
    non_null = read_vec(value);
  } else if (alloc_type === "Box") {
    let unique = read_field(value, "0");
    non_null = read_unique(unique);
  } else {
    throw new Error(`Unimplemented alloc type: ${alloc_type}`);
  }

  let ptr = non_null.fields[0][1];
  return ptr;
};

let AdtView = ({ value }: { value: MAdt }) => {
  let pathCtx = useContext(PathContext);
  let config = useContext(ConfigContext);

  let ptr = specialPtr(value);
  if (ptr && !config.concreteTypes) return <ValueView value={ptr} />;

  if (value.name === "Iter" && !config.concreteTypes) {
    let non_null = read_field(value, "ptr");
    let ptr = non_null.fields[0][1];
    return <ValueView value={ptr} />;
  }

  let adtName = value.variant ?? value.name;

  let isTuple = value.fields.length > 0 && value.fields[0][0] === "0";

  if (isTuple && value.fields.length === 1) {
    let path = [...pathCtx, "field", "0"];
    let field = value.fields[0][1];
    let inner = <ValueView value={field} />;
    let smallInside =
      field.type === "Adt" &&
      !config.concreteTypes &&
      specialPtr(field.value) !== undefined;
    return (
      <SubvalueView
        segment={{ type: "Field", value: 0 }}
        path={path}
        element="span"
      >
        {smallInside ? (
          <>
            {adtName}({inner})
          </>
        ) : (
          <>
            {" "}
            {adtName} /&nbsp;{inner}
          </>
        )}
      </SubvalueView>
    );
  }

  let cells = value.fields.map(([k, v], i) => (
    <SubvalueView
      key={k}
      segment={{ type: "Field", value: i }}
      path={[...pathCtx, "field", i.toString()]}
      value={v}
      // A tuple's elements have no names, so those cells shade on their own;
      // a named field hands its label to `SubvalueView` and becomes a row.
      label={isTuple ? undefined : k}
    />
  ));

  return (
    <>
      {adtName}
      <table>
        <tbody>{isTuple ? <tr>{cells}</tr> : cells}</tbody>
      </table>
    </>
  );
};

let PointerView = ({ value: { path, range } }: { value: MPointer }) => {
  let config = useContext(ConfigContext);

  let segment =
    path.segment.type === "Heap"
      ? `heap-${path.segment.value.index}`
      : `stack-${path.segment.value.frame}-${path.segment.value.local}`;

  let parts = [...path.parts];
  let lastPart = _.last(parts);
  let slice =
    lastPart && lastPart.type === "Subslice" ? lastPart.value : undefined;
  if (lastPart && lastPart.type === "Index" && lastPart.value === 0)
    parts.pop();
  let partClass = parts.map(part =>
    part.type === "Index"
      ? `index-${part.value}`
      : part.type === "Field"
        ? `field-${part.value}`
        : part.type === "Subslice"
          ? `index-${part.value[0]}`
          : ""
  );

  let attrs: { [key: string]: string } = {
    "data-point-to": [segment, ...partClass].join("-")
  };
  if (slice) {
    attrs["data-point-to-range"] = [
      segment,
      ...partClass.slice(0, -1),
      `index-${slice[1]}`
    ].join("-");
  }

  let ptrView = (
    <span className="pointer" {...attrs}>
      ●
    </span>
  );

  return config.concreteTypes && range ? (
    <table>
      <tbody>
        <tr>
          <td>ptr</td>
          <td>{ptrView}</td>
        </tr>
        <tr>
          <td>len</td>
          <td>{range.toString()}</td>
        </tr>
      </tbody>
    </table>
  ) : (
    ptrView
  );
};

/// Wraps a value that is itself the thing that was moved, without a finer
/// place to say so.
let MovedWrapper: React.FC<React.PropsWithChildren<{ moved: boolean }>> = ({
  moved,
  children
}) => (moved ? <span className="moved">{children}</span> : <>{children}</>);

let ValueView = ({ value }: { value: MValue }) => {
  let pathCtx = useContext(PathContext);
  let error = useContext(ErrorContext);

  // A move that reaches into something the diagram does not lay out
  // field-by-field -- inside a pointer, or an array's abbreviated middle --
  // has no cell of its own to shade, so it shades this value as a whole. That
  // is what the old behaviour did for every partial move.
  let composite =
    value.type === "Adt" || value.type === "Tuple" || value.type === "Array";
  let stranded = useContext(MovedPathsContext).length > 0 && !composite;

  return (
    <MovedWrapper moved={stranded}>
      {value.type === "Bool" ||
      value.type === "Uint" ||
      value.type === "Int" ||
      value.type === "Float" ? (
        value.value.toString()
      ) : value.type === "Char" ? (
        String.fromCharCode(value.value).replace(" ", "\u00A0")
      ) : value.type === "Tuple" ? (
        <>
          <table>
            <tbody>
              <tr>
                {value.value.map((v, i) => (
                  <SubvalueView
                    key={i}
                    segment={{ type: "Field", value: i }}
                    path={[...pathCtx, "field", i.toString()]}
                    value={v}
                  />
                ))}
              </tr>
            </tbody>
          </table>
        </>
      ) : value.type === "Adt" ? (
        <AdtView value={value.value} />
      ) : value.type === "Pointer" ? (
        <PointerView value={value.value} />
      ) : value.type === "Array" ? (
        <AbbreviatedView value={value.value} />
      ) : value.type === "Unallocated" ? (
        (() => {
          let isError =
            error &&
            error.type === "PointerUseAfterFree" &&
            error.value.alloc_id === value.value.alloc_id;
          return (
            <span className={classNames("unallocated", { error: isError })}>
              ⦻
            </span>
          );
        })()
      ) : (
        <>TODO</>
      )}
    </MovedWrapper>
  );
};

let LocalsView = ({ index, locals }: { index: number; locals: MLocal[] }) =>
  locals.length === 0 ? (
    <div className="locals empty-frame">(empty frame)</div>
  ) : (
    <table className="locals">
      <tbody>
        {locals.map(({ name, value, moved_paths }, i) => {
          let path = ["stack", index.toString(), name];

          // An empty path is the local itself, so the whole row is shaded --
          // name included. Anything deeper is a partial move, and is routed
          // into the value so that only the field that left is shaded.
          let isMoved = moved_paths.some(p => p.length === 0);
          let partialPaths = isMoved
            ? []
            : moved_paths.filter(p => p.length > 0);

          return (
            <tr key={i} className={classNames({ moved: isMoved })}>
              <td>{name}</td>
              <td className={path.join("-")} data-connector="right">
                <PathContext.Provider value={path}>
                  <MovedPathsContext.Provider value={partialPaths}>
                    <ValueView value={value} />
                  </MovedPathsContext.Provider>
                </PathContext.Provider>
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );

let Header: React.FC<React.PropsWithChildren<{ className: string }>> = ({
  children,
  className
}) => (
  <div className={`header ${className ?? ""}`}>
    <div className="header-text">{children}</div>
    <div className="header-bg" />
  </div>
);

let FrameView = ({
  index,
  frame
}: {
  index: number;
  frame: MFrame<CharRange>;
}) => {
  let code = useContext(CodeContext);
  let snippet = codeRange(code!, frame.location);
  return (
    <div className="frame">
      <Header className="frame-header">{frame.name}</Header>
      {DEBUG ? <pre>{snippet}</pre> : null}
      <LocalsView index={index} locals={frame.locals} />
    </div>
  );
};

let StackView = ({ stack }: { stack: MStack<CharRange> }) => (
  <div className="memory stack">
    <Header className="memory-header">Stack</Header>
    <div className="frames">
      {stack.frames.map((frame, i) => (
        <FrameView key={i} index={i} frame={frame} />
      ))}
    </div>
  </div>
);

let HeapView = ({ heap }: { heap: MHeap }) => (
  <div className="memory heap">
    <Header className="memory-header">Heap</Header>
    <table>
      <tbody>
        {heap.locations.map((value, i) => {
          let path = ["heap", i.toString()];
          return (
            <tr key={i}>
              <td className={path.join("-")} data-connector="left">
                <PathContext.Provider value={path}>
                  <ValueView value={value} />
                </PathContext.Provider>
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  </div>
);

(LeaderLine as any).positionByWindowResize = false;

// to_rgb = lambda p: [f'rgba({int(r*255)}, {int(g*255)}, {int(b*255)}, 1)' for (r, g, b) in p]
const PALETTE = {
  // to_rgb(sns.color_palette("rocket", 15)[:6])
  light: [
    "rgba(24, 15, 41, 1)",
    "rgba(47, 23, 57, 1)",
    "rgba(71, 28, 72, 1)",
    "rgba(97, 30, 82, 1)",
    "rgba(123, 30, 89, 1)",
    "rgba(150, 27, 91, 1)"
  ],
  // to_rgb(sns.color_palette("rocket_r", 20, desat=0.5)[:6])
  dark: [
    "rgba(234, 219, 207, 1)",
    "rgba(227, 203, 187, 1)",
    "rgba(220, 187, 168, 1)",
    "rgba(214, 172, 151, 1)",
    "rgba(208, 156, 136, 1)",
    "rgba(202, 140, 121, 1)"
  ]
};

let renderArrows = (
  containerRef: React.RefObject<HTMLDivElement>,
  stepContainerRef: React.RefObject<HTMLDivElement>,
  arrowContainerRef: React.RefObject<HTMLDivElement>
) => {
  useEffect(() => {
    let stepContainer = stepContainerRef.current!;
    let arrowContainer = arrowContainerRef.current!;

    let sources = stepContainer.querySelectorAll<HTMLSpanElement>(".pointer");

    // TODO: this should be configurable from the embed script, not directly
    // inside aquascope-editor
    let mdbookEmbed = getComputedStyle(document.body).getPropertyValue(
      "--inline-code-color"
    );

    let query = (sel: string): HTMLElement => {
      let dst = stepContainer.querySelector<HTMLElement>(`.${CSS.escape(sel)}`);
      if (!dst)
        throw new Error(
          `Could not find endpoint for pointer selector: ${CSS.escape(sel)}`
        );
      return dst;
    };

    type MemoryRegion = "stack" | "heap";
    let getMemoryRegion = (el: HTMLElement): MemoryRegion =>
      el.closest(".heap") !== null ? "heap" : "stack";

    interface Pointer {
      src: HTMLElement;
      dst: HTMLElement;
      dstSel: string;
      dstRange?: HTMLElement;
      endSocket: SocketType;
      dstIndex: number;
      group: {
        srcRegion: MemoryRegion;
        dstRegion: MemoryRegion;
      };
    }

    // First, collect metadata about all the pointers we're rendering
    // like what HTML elements are pointed, and what region of the digram
    // they lie in.
    let dstCounts: { [sel: string]: number } = {};
    let pointers = Array.from(sources).map<Pointer>(src => {
      let dstSel = src.dataset.pointTo!;
      let dst = query(dstSel);
      let dstRange = src.dataset.pointToRange
        ? query(src.dataset.pointToRange)
        : undefined;
      let endSocket = dst.dataset.connector as SocketType;
      let group = {
        srcRegion: getMemoryRegion(src),
        dstRegion: getMemoryRegion(dst)
      };

      if (!(dstSel in dstCounts)) dstCounts[dstSel] = 0;
      let dstIndex = dstCounts[dstSel];
      dstCounts[dstSel] += 1;

      return { src, dst, dstRange, dstSel, endSocket, dstIndex, group };
    });

    // Then, group the pointers by their regions.
    // That way we know how many pointers are e.g. pointing from stack->heap
    // so we can stagger them correctly.
    let groups = _.groupBy(pointers, "group");

    interface RenderedPointer {
      line: LeaderLineCls;
      svgElements: Element[];
    }

    // Then we render each pointer, conditioned on its group.
    let renderPtr = (ptr: Pointer, i: number): RenderedPointer | undefined => {
      try {
        let { srcRegion, dstRegion } = ptr.group;

        // Heap -> stack pointers should start on the left and
        // everything else starts on the right
        let startSocket: SocketType =
          srcRegion === "heap" && dstRegion === "stack" ? "left" : "right";

        let dstAnchor: AnchorAttachment;
        if (ptr.dstRange) {
          // Pointers to ranges (eg string slices) need an area anchor
          // corresponding to the range of the slice
          // TODO: this doesn't handle abbreviations
          dstAnchor = LeaderLine.areaAnchor(ptr.dst, {
            shape: "rect",
            width:
              ptr.dstRange.offsetLeft +
              ptr.dst.offsetWidth -
              ptr.dst.offsetLeft,
            height: 2,
            y: "100%",
            fillColor: mdbookEmbed ? "var(--search-mark-bg)" : "red"
          });
        } else if (srcRegion === "stack" && dstRegion === "stack") {
          // Stack -> stack pointers should point a little below the middle
          // to avoid conflicting with outgoing pointers.
          dstAnchor = LeaderLine.pointAnchor(ptr.dst, { x: "100%", y: "75%" });
        } else if (ptr.endSocket === "bottom") {
          dstAnchor = ptr.dst;
        } else {
          let x = dstRegion === "stack" ? 100 : 0;

          // Everything else should get evenly spaced around the
          // middle of the endpoint
          let y = evenlySpaceAround({
            center: 50,
            spacing: 30,
            index: ptr.dstIndex,
            total: dstCounts[ptr.dstSel] - 1
          });

          dstAnchor = LeaderLine.pointAnchor(ptr.dst, {
            x: `${x}%`,
            y: `${y}%`
          });
        }

        const MDBOOK_DARK_THEMES = ["navy", "coal", "ayu"];
        let isDark = MDBOOK_DARK_THEMES.some(s =>
          document.documentElement.classList.contains(s)
        );
        let theme: "dark" | "light" = isDark ? "dark" : "light";
        let palette = PALETTE[theme];
        let color = palette[i % palette.length];

        let srcIsMoved = ptr.src.closest(".moved") !== null;
        let moveOpacity = isDark ? 0.5 : 0.3;
        if (srcIsMoved) color = color.replace(", 1)", `, ${moveOpacity})`);

        let startSocketGravity = undefined;
        let endSocketGravity = undefined;
        if (ptr.group.srcRegion === "stack" && ptr.group.dstRegion === "heap") {
          startSocketGravity = 60;
          endSocketGravity = 100 - i * 10;
        }

        let line = new LeaderLine(ptr.src, dstAnchor, {
          color,
          size: 1,
          endPlugSize: 2,
          startSocket,
          endSocket: ptr.endSocket,
          startSocketGravity,
          endSocketGravity
        });

        // Make arrows local to the diagram rather than global in the body
        // See: https://github.com/anseki/leader-line/issues/54
        let svgSelectors = [".leader-line"];
        if (ptr.dstRange) svgSelectors.push(".leader-line-areaAnchor");
        let svgElements = svgSelectors.map(sel => {
          let el = document.body.querySelector(`:scope > ${sel}`);
          if (!el) throw new Error(`Missing LineLeader element: ${sel}`);
          return el;
        });

        svgElements.forEach(el => arrowContainer.appendChild(el));

        return { line, svgElements };
      } catch (e: any) {
        console.error("Leader line failed to render", e.stack);
        return undefined;
      }
    };

    let lines = Object.entries(groups)
      .flatMap(([_g, ptrs]) => ptrs.map((ptr, i) => renderPtr(ptr, i)))
      .filter(obj => obj !== undefined) as RenderedPointer[];

    // Lastly, we add a timer to reposition the arrow container
    // if necessary.
    // LeaderLine positions its SVGs in document coordinates, so the container
    // offset must be in that same space. getBoundingClientRect is already
    // relative to the viewport and accounts for every ancestor's scrolling,
    // so adding `container`'s own scroll offsets double-counts them and
    // detaches the arrows by exactly that amount.
    let curCoords = (): [number, number] => {
      let stepBox = stepContainer.getBoundingClientRect();
      let x = stepBox.left + window.scrollX;
      let y = stepBox.top + window.scrollY;
      return [x, y];
    };

    let positionArrowContainer = (x: number, y: number) => {
      lines.forEach(({ line }) => line.position());
      arrowContainer.style.transform = `translate(-${x}px, -${y}px)`;
    };
    let lastCoords = curCoords();
    positionArrowContainer(...lastCoords);

    let interval = setInterval(() => {
      let newCoords = curCoords();
      if (newCoords[0] !== lastCoords[0] || newCoords[1] !== lastCoords[1]) {
        positionArrowContainer(...newCoords);
      }
      lastCoords = newCoords;
    }, 300);

    return () => {
      clearInterval(interval);
      lines.forEach(({ svgElements }) => {
        svgElements.forEach(el => {
          el.parentNode!.removeChild(el);
        });
      });
    };
  });
  // Note: this effect must be re-run every time, since children *might* change
  // their contents and invalidate DOM references held within LineLeader
};

let StepView = ({
  step,
  index,
  containerRef,
  visible
}: {
  step: MStep<CharRange>;
  index: number;
  containerRef: React.RefObject<HTMLDivElement>;
  visible: boolean;
}) => {
  let stepContainerRef = useRef<HTMLDivElement>(null);
  let arrowContainerRef = useRef<HTMLDivElement>(null);
  let error = useContext(ErrorContext);
  renderArrows(containerRef, stepContainerRef, arrowContainerRef);

  return (
    <div className="step" style={{ opacity: visible ? "1" : "0" }}>
      <div className="step-header">
        <StepMarkerView
          index={index}
          fail={error !== undefined}
          visible={visible}
        />
        {error !== undefined ? (
          <span className="undefined-behavior">
            undefined behavior:{" "}
            {error.type === "PointerUseAfterFree" ? (
              <>pointer used after its pointee is freed</>
            ) : (
              error.value
            )}
          </span>
        ) : null}
      </div>
      <div className="memory-container" ref={stepContainerRef}>
        <div className="arrow-container" ref={arrowContainerRef} />
        <div className="memory-container-flex">
          <StackView stack={step.stack} />
          {step.heap.locations.length > 0 ? (
            <HeapView heap={step.heap} />
          ) : null}
        </div>
      </div>
    </div>
  );
};

let InterpreterView = ({
  trace,
  config,
  onStepUpdated
}: {
  trace: MTrace<CharRange>;
  config?: InterpreterConfig;
  onStepUpdated?: (step: number) => void;
}) => {
  let ref = useRef<HTMLDivElement>(null);
  let [concreteTypes, setConcreteTypes] = useState(
    config?.concreteTypes ?? false
  );
  let [buttonVisible, setButtonVisible] = useState(false);
  let [currentStep, setCurrentStep] = useState(0);

  let flexDirection: CSSProperties["flexDirection"] = config?.horizontal
    ? "row"
    : "column";

  let controls = config?.interpreterControls || false;

  return (
    <ConfigContext.Provider value={{ ...config, concreteTypes: concreteTypes }}>
      <div
        ref={ref}
        className="interpreter"
        style={{ flexDirection }}
        onMouseEnter={() => setButtonVisible(true)}
        onMouseLeave={() => setButtonVisible(false)}
      >
        <div className="actions" style={{ opacity: buttonVisible ? "1" : "0" }}>
          {controls ? (
            <button
              type="button"
              className="step-button"
              onClick={() => {
                if (currentStep === 0) return;
                let nextStep = currentStep - 1;
                setCurrentStep(nextStep);
                onStepUpdated?.(nextStep);
              }}
            >
              <i className="fa fa-step-backward step-back" />
            </button>
          ) : null}
          {controls ? (
            <button
              type="button"
              className="step-button"
              onClick={() => {
                if (currentStep === trace.steps.length) return;
                let nextStep = currentStep + 1;
                setCurrentStep(nextStep);
                onStepUpdated?.(nextStep);
              }}
            >
              <i className="fa fa-step-forward step-next" />
            </button>
          ) : null}
          <button
            type="button"
            className={classNames("concrete-types", { active: concreteTypes })}
            onClick={() => setConcreteTypes(!concreteTypes)}
          >
            <i className="fa fa-binoculars" />
          </button>
        </div>
        {trace.steps.map((step, i) => {
          let error =
            i === trace.steps.length - 1 && trace.result.type === "Error"
              ? trace.result.value
              : undefined;
          return (
            <ErrorContext.Provider key={i} value={error}>
              <StepView
                index={i}
                step={step}
                containerRef={ref}
                visible={!controls || i < currentStep}
              />
            </ErrorContext.Provider>
          );
        })}
      </div>
    </ConfigContext.Provider>
  );
};

let filterSteps = (
  view: EditorView,
  steps: MStep<CharRange>[],
  marks: number[]
): [number[], MStep<CharRange>[]] => {
  let stepsRev = [...steps].reverse();
  let indexedMarks: [number, number, MStep<CharRange>][] = marks.map(idx => {
    let stepRevIdx = stepsRev.findIndex(step => {
      let frame = _.last(step.stack.frames)!;
      let markInFrame =
        linecolToPosition(frame.body_span.start, view.state.doc) <= idx &&
        idx <= linecolToPosition(frame.body_span.end, view.state.doc);
      let markAfterLoc =
        idx > linecolToPosition(frame.location.start, view.state.doc);
      return markInFrame && markAfterLoc;
    });
    if (stepRevIdx === -1)
      throw new Error(
        `Could not find step for range: ${JSON.stringify(idx, undefined, 2)}`
      );
    return [steps.length - stepRevIdx, idx, stepsRev[stepRevIdx]];
  });
  let sortedMarks = _.sortBy(indexedMarks, ([idx]) => idx);
  return [
    sortedMarks.map(([_stepIdx, mark]) => mark),
    sortedMarks.map(([_stepIdx, _mark, step]) => step)
  ];
};

let StepMarkerView = ({
  index,
  fail,
  visible
}: {
  index: number;
  fail: boolean;
  visible: boolean;
}) => {
  return (
    <span
      className={classNames("step-marker", { fail })}
      style={{ opacity: visible ? "1" : "0" }}
    >
      <span>L{index + 1}</span>
    </span>
  );
};

class StepMarkerWidget extends WidgetType {
  constructor(
    readonly index: number,
    readonly fail: boolean,
    readonly visible: boolean
  ) {
    super();
  }

  toDOM() {
    let container = document.createElement("span");
    ReactDOM.createRoot(container).render(
      <StepMarkerView
        index={this.index}
        fail={this.fail}
        visible={this.visible}
      />
    );
    return container;
  }
}

export let markerField = makeDecorationField();

export function renderInterpreter(
  view: EditorView,
  container: HTMLDivElement,
  trace: MTrace<CharRange>,
  config?: InterpreterConfig,
  annotations?: InterpAnnotations
) {
  let root = ReactDOM.createRoot(container);
  let marks = annotations?.state_locations || [];
  let widgetRanges: number[];
  if (marks.length > 0) {
    let [sortedMarks, filteredSteps] = filterSteps(view, trace.steps, marks);
    widgetRanges = sortedMarks;
    trace.steps = filteredSteps;
  } else {
    widgetRanges = trace.steps.map(step =>
      linecolToPosition(_.last(step.stack.frames)!.location.end, view.state.doc)
    );
  }

  let controls = config?.interpreterControls || false;

  let renderStepMarkers = (step: number) => {
    let decos = widgetRanges.map((mark, i) =>
      Decoration.widget({
        widget: new StepMarkerWidget(
          i,
          i === trace.steps.length - 1 && trace.result.type === "Error",
          !controls || i < step
        )
      }).range(mark)
    );

    view.dispatch({
      effects: [markerField.setEffect.of(decos)]
    });
  };

  renderStepMarkers(0);

  root.render(
    <CodeContext.Provider value={view}>
      <InterpreterView
        trace={trace}
        config={config}
        onStepUpdated={step => renderStepMarkers(step)}
      />
    </CodeContext.Provider>
  );
}
