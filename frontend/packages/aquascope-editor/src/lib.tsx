import { rust } from "@codemirror/lang-rust";
import { indentUnit } from "@codemirror/language";
import { Compartment, EditorState, Extension } from "@codemirror/state";
import { EditorView, ViewUpdate } from "@codemirror/view";
import _ from "lodash";
import React, { useEffect, useState } from "react";
import ReactDOM from "react-dom/client";

import { boundariesField } from "./editor-utils/boundaries";
import {
  InterpreterConfig,
  markerField,
  renderInterpreter,
} from "./editor-utils/interpreter";
import {
  IconField,
  hiddenLines,
  hideLine,
  loanFactsField,
} from "./editor-utils/misc";
import {
  PermissionsCfg,
  PermissionsDecorations,
  makePermissionsDecorations,
  renderPermissions,
} from "./editor-utils/permissions";
import { stepField } from "./editor-utils/stepper";
import "./styles.scss";
import {
  AnalysisFacts,
  AnalysisOutput,
  AquascopeAnnotations,
  BackendError,
  CharRange,
  InterpAnnotations,
  MTrace,
} from "./types";

export * as types from "./types";

const DEFAULT_SERVER_URL = new URL("http://127.0.0.1:8008");

export type Result<T> = { Ok: T } | { Err: BackendError };

// XXX this extra server response type is really
// annoying and I'd like to get rid of it. This change would
// require modifying how command output is read from the spawned
// docker container on the backend.
type ServerResponse = {
  success: boolean;
  stdout: string;
  stderr: string;
};

export const defaultCodeExample: string = `
fn main() {
  let mut v = vec![1, 2, 3];
  let n = &v[0];
  v.push(0);
  let x = *n;
}
`.trim();

let readOnly = new Compartment();
let mainKeybinding = new Compartment();

type ButtonName = "copy" | "eye" | "step-forward" | "step-backward" | "run";

let CopyButton = ({ view }: { view: EditorView }) => (
  <button
    className="cm-button"
    onClick={() => {
      let contents = view.state.doc.toJSON().join("\n");
      navigator.clipboard.writeText(contents);
    }}
  >
    <i className="fa fa-copy" />
  </button>
);

let HideButton = ({ container }: { container: HTMLDivElement }) => {
  let [hidden, setHidden] = useState(true);
  useEffect(() => {
    if (!hidden) container.classList.add("show-hidden");
    else container.classList.remove("show-hidden");
  }, [hidden]);
  return (
    <button className="cm-button" onClick={() => setHidden(!hidden)}>
      <i className={`fa ${hidden ? "fa-eye" : "fa-eye-slash"}`} />
    </button>
  );
};

let StepForwardButton = ({ onClick }: { onClick?: () => void }) => {
  return (
    <button className="cm-button step-next" onClick={() => onClick?.()}>
      <i className="fa fa-step-forward" />
    </button>
  );
};

let StepBackwardButton = ({ onClick }: { onClick?: () => void }) => {
  return (
    <button className="cm-button step-back" onClick={() => onClick?.()}>
      <i className="fa fa-step-backward" />
    </button>
  );
};

let RunButton = ({
  container,
  view,
}: {
  container: HTMLDivElement;
  view: EditorView;
}) => {
  return (
    <button
      className="cm-button"
      onClick={() => run_rust_code(container, view)}
    >
      <i className="fa fa-play" />
    </button>
  );
};

let resetMarkedRangesOnEdit = EditorView.updateListener.of(
  (upd: ViewUpdate) => {
    if (upd.docChanged) {
      upd.view.dispatch({
        effects: [
          boundariesField.setEffect.of([]),
          stepField.setEffect.of([]),
          markerField.setEffect.of([]),
        ],
      });
    }
  }
);

interface CommonConfig {
  shouldFail?: boolean;
  stepper?: any;
  boundaries?: any;
  stepperControls?: any;
  run?: any;
  interpreterControls?: any;
}

export class Editor {
  private view: EditorView;
  private interpreterContainer: HTMLDivElement;
  private dom: HTMLDivElement;
  private editorContainer: HTMLDivElement;
  private resultContainer: HTMLDivElement;
  private permissionsDecos?: PermissionsDecorations;
  private metaContainer: ReactDOM.Root;
  private buttons: Set<ButtonName>;
  private shouldFail: boolean = false;
  private currentStep?: number;
  private annotations?: AquascopeAnnotations;
  private results: AnalysisOutput[] = [];
  private config?: CommonConfig & object;

  public constructor(
    dom: HTMLDivElement,
    readonly setup: Extension,
    readonly reportStdErr: (err: BackendError) => void = function (err) {
      console.error("An error occurred: ");
      console.error(err);
    },
    code: string = defaultCodeExample,
    readonly serverUrl: URL = DEFAULT_SERVER_URL,
    readonly noInteract: boolean = false,
    readonly shouldFailHtml: string = "This code does not compile!",
    readonly buttonList: ButtonName[] = []
  ) {
    this.buttons = new Set(buttonList);

    let initialState = EditorState.create({
      doc: code,
      extensions: [
        mainKeybinding.of(setup),
        readOnly.of(EditorState.readOnly.of(noInteract)),
        EditorView.editable.of(!noInteract),
        resetMarkedRangesOnEdit,
        setup,
        rust(),
        indentUnit.of("  "),
        hiddenLines,
        loanFactsField,
        boundariesField.field,
        stepField.field,
        markerField.field,
      ],
    });

    this.editorContainer = document.createElement("div");
    this.view = new EditorView({
      state: initialState,
      parent: this.editorContainer,
    });

    let buttonContainer = document.createElement("div");
    this.metaContainer = ReactDOM.createRoot(buttonContainer);
    this.renderMeta();

    this.dom = dom;

    this.editorContainer.appendChild(buttonContainer);

    this.interpreterContainer = document.createElement("div");
    this.resultContainer = document.createElement("div");
    this.resultContainer.className = "result-container";

    this.dom.appendChild(this.editorContainer);
    this.dom.appendChild(this.interpreterContainer);
    this.dom.appendChild(this.resultContainer);
  }

  renderMeta() {
    this.metaContainer.render(
      <div className="meta-container">
        <div className="top-right">
          {Array.from(this.buttons).map((button, i) =>
            button == "copy" ? (
              <CopyButton key={i} view={this.view} />
            ) : button == "eye" ? (
              <HideButton key={i} container={this.editorContainer} />
            ) : button == "step-forward" ? (
              <StepForwardButton
                key={i}
                onClick={() => {
                  if (
                    this.currentStep === undefined ||
                    this.currentStep === this.maxPermissionSteps()
                  )
                    return;
                  this.currentStep += 1;
                  this.reRenderPermissions();
                }}
              />
            ) : button == "step-backward" ? (
              <StepBackwardButton
                key={i}
                onClick={() => {
                  if (this.currentStep === undefined || this.currentStep === 0)
                    return;
                  this.currentStep -= 1;
                  this.reRenderPermissions();
                }}
              />
            ) : button == "run" ? (
              <RunButton container={this.resultContainer} view={this.view} />
            ) : null
          )}
        </div>
        {this.shouldFail ? (
          <div dangerouslySetInnerHTML={{ __html: this.shouldFailHtml }} />
        ) : null}
      </div>
    );
  }

  public getCurrentCode(): string {
    return this.view.state.doc.toString();
  }

  public reconfigure(extensions: Extension[]): void {
    this.view.dispatch({
      effects: [mainKeybinding.reconfigure([...extensions, this.setup])],
    });
  }

  public removeIconField<B, T, F extends IconField<B, T>>(f: F) {
    this.view.dispatch({
      effects: [f.effectType.of([])],
    });
  }

  public addPermissionsField<B, T, F extends IconField<B, T>>(
    f: F,
    methodCallPoints: Array<B>,
    facts: AnalysisFacts
  ) {
    let newEffects = methodCallPoints.map(v => f.fromOutput(v, facts));
    this.view.dispatch({
      effects: [f.effectType.of(newEffects)],
    });
  }

  public async renderPermissions(cfg?: PermissionsCfg) {
    // TODO: the permissions Decos are no longer removed on update
    // so we have to recompute every time.
    if (this.permissionsDecos === undefined) {
      await this.renderOperation("permissions", {
        config: cfg,
      });
    }

    renderPermissions(this.view, this.permissionsDecos, cfg);
  }

  // Actions to communicate with the aquascope server
  async callBackendWithCode(
    endpoint: string,
    config?: any
  ): Promise<ServerResponse> {
    let inEditor = this.getCurrentCode();
    let endpointUrl = new URL(endpoint, this.serverUrl);
    let serverResponseRaw = await fetch(endpointUrl, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        code: inEditor,
        config,
      }),
    });
    let serverResponse: ServerResponse = await serverResponseRaw.json();
    return serverResponse;
  }

  renderInterpreter(
    trace: MTrace<CharRange>,
    config?: InterpreterConfig,
    annotations?: InterpAnnotations
  ) {
    if (config && config.hideCode) {
      this.view.destroy();
      this.metaContainer.unmount();
    }

    renderInterpreter(
      this.view,
      this.interpreterContainer,
      trace,
      config,
      annotations
    );
  }

  async renderOperation(
    operation: string,
    {
      response,
      config,
      annotations,
    }: {
      response?: Result<any>;
      config?: CommonConfig & object;
      annotations?: AquascopeAnnotations;
    } = {}
  ) {
    console.debug(`Rendering operation: ${operation}`);

    if (!response) {
      let serverResponse = await this.callBackendWithCode(operation, config);
      if (serverResponse.success) {
        response = JSON.parse(serverResponse.stdout);
        this.reportStdErr({
          type: "ServerStderr",
          error: serverResponse.stderr,
        });
      } else {
        return this.reportStdErr({
          type: "ServerStderr",
          error: serverResponse.stderr,
        });
      }
    }

    if (
      annotations &&
      annotations.hidden_lines &&
      annotations.hidden_lines.length > 0
    ) {
      this.view.dispatch({
        effects: annotations.hidden_lines.map(line => hideLine.of({ line })),
      });
      this.buttons.add("eye");
    }

    if (config?.stepperControls === "true") {
      this.buttons.add("step-backward");
      this.buttons.add("step-forward");
      this.currentStep = 0;
    }

    if (config?.run === "true") {
      this.buttons.add("run");
    }

    if (config?.shouldFail) {
      this.shouldFail = true;
    }

    this.renderMeta();

    if (operation == "interpreter") {
      if ("Ok" in response!) {
        this.renderInterpreter(response.Ok, config as any, annotations?.interp);
      } else {
        this.reportStdErr(response!.Err);
      }
    } else if (operation == "permissions") {
      // The permissions analysis results are sent as an array of
      // body analyses. Each body could have analyzed successfully,
      // or had a
      // 1. analysis error
      // 2. build error
      // A build error signifies that something went wrong *before*
      // our analysis was run. This should be reported to the user,
      // currently, information is available on stderr but nothing
      // more specific (or visual) is given TODO.
      // For an analysis error, this is something that went wrong
      // internally, usually means a feature was used that we don't support
      // or something actually went terribly wrong. These should be logged
      // somewhere, but the user should also be prompted to open a GitHub issue.
      let cast = response as any as Result<AnalysisOutput>[];
      let results: AnalysisOutput[] = [];

      for (var res of cast) {
        if ("Ok" in res) {
          results.push(res.Ok);
        } else {
          this.reportStdErr(res.Err);
        }
      }
      this.annotations = annotations;
      this.results = results;
      this.config = config;
      this.reRenderPermissions();
    }
  }

  reRenderPermissions() {
    this.permissionsDecos = makePermissionsDecorations(
      this.view,
      this.results,
      this.annotations,
      this.currentStep
    );
    renderPermissions(this.view, this.permissionsDecos, this.config);
  }

  maxPermissionSteps(): number {
    return Math.max(...this.results.map(result => result.steps.length));
  }
}

function run_rust_code(resultContainer: HTMLDivElement, view: EditorView) {
  resultContainer.innerHTML =
    '<pre><code class="result hljs language-bash">Running...</code></pre>';
  let resultCode: any = resultContainer.querySelector(".result");
  if (!resultCode) {
    return;
  }
  let text = view.state.doc.toJSON().join("\n");
  var params = {
    version: "stable",
    optimize: "0",
    code: text,
    edition: "2021",
  };

  fetch("https://play.rust-lang.org/evaluate.json", {
    headers: {
      "Content-Type": "application/json",
    },
    method: "POST",
    mode: "cors",
    body: JSON.stringify(params),
  })
    .then((response: any) => response.json())
    .then((response: any) => {
      if (response.result.trim() === "") {
        resultCode.innerText = "No output";
        resultCode.classList.add("result-no-output");
      } else {
        resultCode.innerHTML = response.result;
        resultCode.classList.remove("result-no-output");
      }
    })
    .catch(
      error =>
        (resultCode.innerText = "Playground Communication: " + error.message)
    );
}
