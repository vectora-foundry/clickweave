import type React from "react";
import { AiStepEditor } from "./AiStepEditor";
import { AppDebugKitOpEditor } from "./AppDebugKitOpEditor";
import { ClickEditor } from "./ClickEditor";
import { EndLoopEditor } from "./EndLoopEditor";
import { FindImageEditor } from "./FindImageEditor";
import { HoverEditor } from "./HoverEditor";
import { FindTextEditor } from "./FindTextEditor";
import { FocusWindowEditor } from "./FocusWindowEditor";
import { IfEditor } from "./IfEditor";
import { ListWindowsEditor } from "./ListWindowsEditor";
import { LoopEditor } from "./LoopEditor";
import { McpToolCallEditor } from "./McpToolCallEditor";
import { PressKeyEditor } from "./PressKeyEditor";
import { ScrollEditor } from "./ScrollEditor";
import { SwitchEditor } from "./SwitchEditor";
import { TakeScreenshotEditor } from "./TakeScreenshotEditor";
import { TypeTextEditor } from "./TypeTextEditor";
import type { NodeEditorProps } from "./types";

export type { NodeEditorProps } from "./types";
export { optionalString } from "./types";

export const editorRegistry: Record<string, React.FC<NodeEditorProps>> = {
  AiStep: AiStepEditor,
  AppDebugKitOp: AppDebugKitOpEditor,
  Click: ClickEditor,
  EndLoop: EndLoopEditor,
  FindImage: FindImageEditor,
  FindText: FindTextEditor,
  FocusWindow: FocusWindowEditor,
  Hover: HoverEditor,
  If: IfEditor,
  ListWindows: ListWindowsEditor,
  Loop: LoopEditor,
  McpToolCall: McpToolCallEditor,
  PressKey: PressKeyEditor,
  Scroll: ScrollEditor,
  Switch: SwitchEditor,
  TakeScreenshot: TakeScreenshotEditor,
  TypeText: TypeTextEditor,
};
