Refactoring Plan: platform/app-web Optimization
Objective
Optimize platform/app-web for improved First Contentful Paint (FCP), Total Blocking Time (TBT), and code maintainability by reducing monolithic structural overhead and optimizing asset delivery.
Steps
1. Extract CSS to Static Asset
- Action: Move the content of the APP_STYLE string constant to a new file: platform/app-web/dist/style.css.
- Action: Update the App component rsx! macro to load the stylesheet via a <link rel="stylesheet" href="style.css" /> tag instead of injecting it via the style { {APP_STYLE} } block.
- Justification: Eliminates the latency cost of CSS-in-Rust injection during WASM initialization. Enables the browser to parse and apply styles in parallel with the WASM download, improving FCP. Facilitates browser-native caching of the stylesheet.
2. Refactor State Management (Granular Signals)
- Action: Decompose the global ShellState struct into smaller, independent Signal instances (e.g., identity_signal, session_state_signal, input_signals).
- Action: Refactor UI components to consume only the specific signals required for their rendering.
- Justification: Eliminates unnecessary full-app re-renders caused by cloning the entire ShellState on every interaction. Reduces CPU load and TBT by ensuring VDOM diffing only occurs for impacted components.
3. Component Modularization
- Action: Extract individual view components (e.g., LobbyView, PhaseView, ControlsView) from main.rs into discrete modules within src/.
- Action: Refactor data flow to pass required signals as props to sub-components, reducing reliance on top-level global state access.
- Justification: Enhances maintainability and readability. Allows the Rust compiler to perform more effective dead-code elimination and optimization (tree-shaking) on the generated WASM binary.
Risks and Considerations
- Reactivity: Step 2 requires precise identification of state dependencies to maintain correct UI synchronization.
- Data Flow: Step 3 necessitates refactoring the data flow, potentially increasing the number of props passed between components.
