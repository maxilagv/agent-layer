//! The terminal command catalog: aliases, completion and help share this source.
pub struct Command {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub help: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/help",
        aliases: &["/ayuda", "/?"],
        usage: "/help",
        help: "Ver la ayuda",
    },
    Command {
        name: "/new",
        aliases: &["/nuevo"],
        usage: "/new",
        help: "Nueva sesión",
    },
    Command {
        name: "/clear",
        aliases: &["/limpiar"],
        usage: "/clear",
        help: "Limpiar la pantalla",
    },
    Command {
        name: "/exit",
        aliases: &["/salir", "/quit"],
        usage: "/exit",
        help: "Salir y desconectar la vista",
    },
    Command {
        name: "/context",
        aliases: &[],
        usage: "/context",
        help: "Consultar contexto estimado",
    },
    Command {
        name: "/compact",
        aliases: &[],
        usage: "/compact",
        help: "Guardar checkpoint mecánico",
    },
    Command {
        name: "/effort",
        aliases: &[],
        usage: "/effort [auto|low|medium|high|xhigh]",
        help: "Consultar o cambiar esfuerzo",
    },
    Command {
        name: "/plan",
        aliases: &[],
        usage: "/plan <qué construir>",
        help: "Planificar primero: escribe docs/plan/ y pide aprobación",
    },
    Command {
        name: "/build",
        aliases: &[],
        usage: "/build [notas]",
        help: "Construir el plan aprobado de docs/plan/",
    },
    Command {
        name: "/budget",
        aliases: &[],
        usage: "/budget [dólares|off]",
        help: "Presupuesto de las próximas tareas",
    },
    Command {
        name: "/execution",
        aliases: &[],
        usage: "/execution [individual|delegated|cooperative]",
        help: "Modo de próximas tareas",
    },
    Command {
        name: "/team",
        aliases: &[],
        usage: "/team [start|attach|members|messages|work|findings|objections|evidence|decisions|show|message|pause|resume|close|cancel|priority|export|toggle]",
        help: "Inspeccionar y dirigir un equipo",
    },
];

pub fn canonical(input: &str) -> String {
    if !input.starts_with('/') {
        return input.to_string();
    }
    let (head, tail) = input.split_once(char::is_whitespace).unwrap_or((input, ""));
    let name = COMMANDS
        .iter()
        .find(|c| c.name == head || c.aliases.contains(&head))
        .map_or(head, |c| c.name);
    if tail.is_empty() {
        name.into()
    } else {
        format!("{name} {}", tail.trim_start())
    }
}

pub fn complete(prefix: &str) -> Option<&'static str> {
    COMMANDS
        .iter()
        .find(|c| c.name.starts_with(prefix))
        .map(|c| c.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_has_unique_names_and_aliases() {
        let mut names = std::collections::BTreeSet::new();
        for command in COMMANDS {
            assert!(names.insert(command.name));
            assert_eq!(canonical(command.name), command.name);
            assert!(command.usage.starts_with(command.name));
            for alias in command.aliases {
                assert!(names.insert(alias));
                assert_eq!(canonical(alias), command.name);
            }
        }
        assert_eq!(complete("/comp"), Some("/compact"));
        assert_eq!(complete("/missing"), None);
        assert_eq!(
            canonical("keep  spacing\nand lines"),
            "keep  spacing\nand lines"
        );
    }
}
