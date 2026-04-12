import type React from "react";
import { AiStepEditor } from "./AiStepEditor";
import { AppDebugKitOpEditor } from "./AppDebugKitOpEditor";
import { ClickEditor } from "./ClickEditor";
import { FindImageEditor } from "./FindImageEditor";
import { HoverEditor } from "./HoverEditor";
import { FindTextEditor } from "./FindTextEditor";
import { FocusWindowEditor } from "./FocusWindowEditor";
import { FindAppEditor } from "./FindAppEditor";
import { McpToolCallEditor } from "./McpToolCallEditor";
import { PressKeyEditor } from "./PressKeyEditor";
import { ScrollEditor } from "./ScrollEditor";
import { TakeScreenshotEditor } from "./TakeScreenshotEditor";
import { TypeTextEditor } from "./TypeTextEditor";
import type { NodeEditorProps } from "./types";

export type { NodeEditorProps } from "./types";
export { optionalString } from "./types";

export const editorRegistry: Record<string, React.FC<NodeEditorProps>> = {
  AiStep: AiStepEditor,
  AppDebugKitOp: AppDebugKitOpEditor,
  Click: ClickEditor,
  FindImage: FindImageEditor,
  FindText: FindTextEditor,
  FocusWindow: FocusWindowEditor,
  Hover: HoverEditor,
  FindApp: FindAppEditor,
  McpToolCall: McpToolCallEditor,
  PressKey: PressKeyEditor,
  Scroll: ScrollEditor,
  TakeScreenshot: TakeScreenshotEditor,
  TypeText: TypeTextEditor,
};
