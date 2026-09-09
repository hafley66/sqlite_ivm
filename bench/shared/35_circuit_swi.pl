% Dynamic relations and incremental reach mirror bench/swi_reach.pl.
% Bag predicates are untabled and recomputed. Only reach/1 uses incremental
% tabling; the receipt names this distinction for each circuit.
:- use_module(library(http/json)).
:- use_module(library(crypto)).
:- dynamic a/3 as incremental.
:- dynamic b/3 as incremental.
:- dynamic c/3 as incremental.
:- table reach/1 as incremental.
reach(K) :- b(_,K,_).
reach(V) :- a(_,K,V), reach(K).

result(pipeline,[K,V2]) :- a(_,K,V), V>=0, V2 is V*2.
result(fanout_fanin,[K,V]) :- a(_,K,V), V>=0.
result(fanout_fanin,[K,V]) :- a(_,K,V), 0 is V mod 2.
result(distinct,[K,V]) :- a(_,K,V).
result(join,[K,P]) :- a(_,K,V), b(_,K,W), P is V*W.
result(self_join,[K,W]) :- a(_,K,V), a(_,V,W).
result(chain,[K,X]) :- a(_,K,V), b(_,V,W), c(_,W,X).
result(diamond,[K,W]) :- a(_,K,V), b(_,V,W).
result(diamond,[K,W]) :- a(_,K,V), c(_,V,W).
result(semijoin,[K,V]) :- a(_,K,V), once(b(_,K,_)).
result(antijoin,[K,V]) :- a(_,K,V), \+ b(_,K,_).
result(reach_cycle,[K]) :- reach(K).
result(aggregate_churn,[K,N,S]) :-
    findall(Key,(a(_,Key,_),b(_,Key,_)),Ks), sort(Ks,Keys), member(K,Keys),
    findall(P,(a(_,K,V),b(_,K,W),P is V*W),Ps), length(Ps,N), sum_list(Ps,S).

outputs(Family,Rows) :- findall(Row,result(Family,Row),Bag),
    ( Family==distinct -> sort(Bag,Rows) ; msort(Bag,Rows) ).
write_input(W) :- atom_string(Table,W.table), memberchk(Table,[a,b,c]),
    Id=W.id, Head=..[Table,Id,_,_], retractall(Head),
    ( W.row==null -> true ; W.row=[Id,K,V], Fact=..[Table,Id,K,V], assertz(Fact) ).
inputs(Table,Rows) :- Goal=..[Table,Id,K,V],findall([Id,K,V],Goal,Bag),msort(Bag,Rows).
row_text(Prefix,Row) :- format('~w',[Prefix]),forall(member(N,Row),format('\t~d',[N])),nl.
canonical(Prefix,Rows,Text) :- with_output_to(string(Text),forall(member(Row,Rows),row_text(Prefix,Row))).
hash(Text,Hash) :- crypto_data_hash(Text,Hash,[algorithm(sha256),encoding(utf8)]).
emit(Record) :- json_write_dict(current_output,Record,[width(0)]),nl,flush_output.
check(Goal,Label) :- (call(Goal)->true;throw(error(mismatch(Label),_))).
unavailable(Unit,Reason,_{value:null,unit:Unit,unavailable_reason:Reason}).
state_inventory(A,B,C,Rows,Inventory) :-
    length(A,NA),length(B,NB),length(C,NC),length(Rows,NO),NS is NA+NB+NC,Total is NS+NO,
    Reason='SWI internal table/index bytes and incremental-table support state are not exposed by this adapter',
    unavailable(bytes,Reason,Bytes),unavailable(rows,Reason,SupportRows),unavailable(indexes,Reason,Indexes),
    Relations=[_{name:a,kind:'native-relation',role:source,counted_in_totals:true,row_count:_{value:NA,unit:rows,unavailable_reason:null},bytes:_{allocated:Bytes,data:Bytes,index:Bytes}},
      _{name:b,kind:'native-relation',role:source,counted_in_totals:true,row_count:_{value:NB,unit:rows,unavailable_reason:null},bytes:_{allocated:Bytes,data:Bytes,index:Bytes}},
      _{name:c,kind:'native-relation',role:source,counted_in_totals:true,row_count:_{value:NC,unit:rows,unavailable_reason:null},bytes:_{allocated:Bytes,data:Bytes,index:Bytes}},
      _{name:'output-bag',kind:'native-relation',role:result,counted_in_totals:true,row_count:_{value:NO,unit:rows,unavailable_reason:null},bytes:_{allocated:Bytes,data:Bytes,index:Bytes}}],
    Inventory=_{schema_version:1,measured_at:'after-output-validation',outside_timed_region:true,scope:'adapter-observed SWI dynamic source relations and output bag',relations:Relations,
      summary:_{table_count:_{value:0,unit:tables,unavailable_reason:null},index_count:Indexes,native_collection_count:_{value:4,unit:relations,unavailable_reason:null},total_rows:_{value:Total,unit:rows,unavailable_reason:null,partial:true},rows_by_role:_{source:_{value:NS,unit:rows,unavailable_reason:null},result:_{value:NO,unit:rows,unavailable_reason:null},support:SupportRows},table_bytes:Bytes,index_bytes:Bytes,total_relation_bytes:Bytes},
      storage:_{database_file_bytes:_{value:null,unit:bytes,unavailable_reason:'volatile SWI adapter has no database file'},wal_file_bytes:_{value:null,unit:bytes,unavailable_reason:'volatile SWI adapter has no WAL file'},database_allocated_bytes:_{value:null,unit:bytes,unavailable_reason:'volatile SWI adapter has no database allocation'},database_size_scope:'no durable database'},process_memory:_{rss_bytes:_{value:null,unit:bytes,unavailable_reason:'measured by parent runner'}},limitations:[Reason]}.

states([],_,Total,Input,Output) :- emit(_{event:'case-total',status:ok,update_plus_query_ms:Total,final_input_hash:Input,final_checksum:Output,disk:_{database_bytes:0}}).
states([State|Rest],Family,Total,_,_) :-
    get_time(T0),transaction(maplist(write_input,State.writes)),get_time(T1),
    outputs(Family,Rows),get_time(T2),Update is (T1-T0)*1000,Query is (T2-T1)*1000,Combined is Update+Query,
    check(Rows==State.expected.rows,State.name),
    inputs(a,A),inputs(b,B),inputs(c,C),
    msort(State.inputs.a,EA),msort(State.inputs.b,EB),msort(State.inputs.c,EC),
    check(A==EA,inputs_a),check(B==EB,inputs_b),check(C==EC,inputs_c),
    canonical('A',A,AT),canonical('B',B,BT),canonical('C',C,CT),atomics_to_string([AT,BT,CT],InputText),
    hash(InputText,InputHash),canonical('S',Rows,OutputText),hash(OutputText,OutputHash),
    atom_string(InputHash,IH),atom_string(OutputHash,OH),check(IH==State.input_hash,input_hash),check(OH==State.expected.checksum,output_hash),
    length(Rows,Count),length(State.writes,Affected),string_length(OutputText,Bytes),
    state_inventory(A,B,C,Rows,Inventory),
    emit(_{event:mutation,status:ok,state:State.name,exact_input_output_validated:true,input_hash:IH,checksum:OH,
           affected_rows:Affected,output_rows:Count,output_bytes:Bytes,update_transaction_ms:Update,query_compute_ms:Query,update_plus_query_ms:Combined,state_inventory:Inventory}),
    Next is Total+Combined,states(Rest,Family,Next,IH,OH).
main :- get_time(SetupStart), current_prolog_flag(argv,[Path]),
    setup_call_cleanup(open(Path,read,In),json_read_dict(In,Fixture),close(In)),
    atom_string(Family,Fixture.circuit),
    (Family==reach_cycle->Algorithm='SWI incremental tabling';Algorithm='SWI full predicate recomputation'),
    current_prolog_flag(version_data,Version),term_string(Version,VersionString),
    get_time(SetupEnd),SetupMs is (SetupEnd-SetupStart)*1000,
    emit(_{event:'case-setup',status:ok,setup_ms:SetupMs,algorithm:Algorithm,durability:volatile,version:VersionString}),
    states(Fixture.states,Family,0,"","").
:- initialization((catch(main,Error,(print_message(error,Error),halt(1)))->halt(0);halt(1)),main).
