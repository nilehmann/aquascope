import { rust } from "@codemirror/lang-rust";
import { indentUnit } from "@codemirror/language";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView, lineNumbers } from "@codemirror/view";
import { Vim, vim } from "@replit/codemirror-vim";

import { setup } from "./setup";

// A plain Rust editor, for tools that want somewhere to type code but none of
// Aquascope's analysis -- aquascope-reveal's editor modal is the one user.
// Exposed as a global for the same reason as `initAquascopeBlocks`: the
// bundle is an IIFE loaded with a <script> tag.

export interface CodeEditor {
  getCode(): string;
  setCode(code: string): void;
  setVim(on: boolean): void;
  focus(): void;
  destroy(): void;
}

export interface CodeEditorOptions {
  vim?: boolean;
  /** Called on every change to the document. */
  onChange?: (code: string) => void;
}

declare global {
  function aquascopeCodeEditor(
    parent: HTMLElement,
    code: string,
    options?: CodeEditorOptions
  ): CodeEditor;
}

// `:q` and `:wq` have nothing to quit inside a page, so they are turned into
// an event on the editor's DOM for the host to act on. `Vim.defineEx` is
// global, which is why the event carries the editor rather than a callback
// bound to one.
export const QUIT_EVENT = "aquascope-editor-quit";
let quit = (cm: any) => {
  (cm.cm6 as EditorView).dom.dispatchEvent(
    new CustomEvent(QUIT_EVENT, { bubbles: true })
  );
};
Vim.defineEx("quit", "q", quit);
Vim.defineEx("wq", "wq", quit);

window.aquascopeCodeEditor = (parent, code, options = {}) => {
  let keys = new Compartment();
  let view = new EditorView({
    parent,
    state: EditorState.create({
      doc: code,
      extensions: [
        // Before `setup`, so its keymap is consulted first: vim has to see
        // Escape and the normal-mode keys before the default keymap does.
        keys.of(options.vim ? vim() : []),
        lineNumbers(),
        setup,
        rust(),
        indentUnit.of("    "),
        // Code is not prose: no spellchecking, and no grammar extension
        // underlining it or rewriting it -- Grammarly reads the `data-gramm*`
        // attributes, and its newer builds `data-enable-grammarly`.
        EditorView.contentAttributes.of({
          spellcheck: "false",
          autocorrect: "off",
          autocapitalize: "off",
          "data-gramm": "false",
          "data-gramm_editor": "false",
          "data-enable-grammarly": "false"
        }),
        EditorView.updateListener.of(update => {
          if (update.docChanged && options.onChange) {
            options.onChange(update.state.doc.toString());
          }
        })
      ]
    })
  });

  return {
    getCode: () => view.state.doc.toString(),
    setCode: code =>
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: code }
      }),
    setVim: on => view.dispatch({ effects: keys.reconfigure(on ? vim() : []) }),
    focus: () => view.focus(),
    destroy: () => view.destroy()
  };
};
